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
//! phase by phase ([`Phase`]), and takes the peak. Housekeeping comes first: indexed assets not
//! yet verified in the private workspace are copied there ([`Extent::VerifiedCopies`], history
//! roots included), and when the workspace holds many small backing files they are packed into
//! one ([`Extent::CompactionPack`]), which briefly doubles them and then frees the originals.
//! Then the archive is prepared, the destination staged, and the checkpoint published. The
//! previous recovery checkpoint stays until its replacement is published, so its space is never
//! credited. The file a save replaces is credited only after the atomic rename that publishes
//! the new one, which is when its space comes back.
//!
//! Extents that already exist when the operation starts (the open project's private copy,
//! imports already staged, files whose cleanup failed) are already missing from the free space
//! the platform reports, so charging them again would refuse saves that fit. They are not
//! charged; cleanup debt is named in the refusal, since restarting frees it. What an open writes
//! after reading the file (a classic-format file's verified asset copies, an older-format file's
//! rewrite) is known only once it is read, so it is checked then, inside the open
//! ([`check_open_copies`], [`check_rewrite`]), against the space left after the private copy.
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
    /// Indexed assets (read from a project file and not yet copied into the private workspace)
    /// copied and verified there before an archive can be written from them, history-only
    /// roots included.
    VerifiedCopies,
    /// The single pack many small backing files are compacted into before output; it coexists
    /// with them only until it replaces them.
    CompactionPack,
    /// An older-format (or identity-migrated) file rewritten in the current format while opening.
    Rewrite,
    /// On Windows, the second copy of the new checkpoint while it replaces the journal: the
    /// staged checkpoint stays on disk until the replace completes, beside the journal already
    /// holding the new content, while the old journal's space comes back.
    CheckpointReplace,
}
impl Extent {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PreparedArchive => "prepared archive",
            Self::DestinationStage => "new project file",
            Self::FrozenBase => "private copy of the project",
            Self::Checkpoint => "recovery checkpoint",
            Self::VerifiedCopies => "verified copies of the project's media",
            Self::CompactionPack => "compacted media pack",
            Self::Rewrite => "project rewritten in the current format",
            Self::CheckpointReplace => "recovery checkpoint while it replaces the journal",
        }
    }
}

/// When an extent is written, in order. An extent stays until the operation ends unless it is
/// transient (see [`SpaceItem::transient`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Verifying unverified media and compacting small backing files.
    Housekeeping,
    /// Preparing the archive (a save) or the private copy and any rewrite (an open).
    Prepare,
    /// Writing the destination beside the file it replaces.
    Stage,
    /// Publishing: the destination is renamed over the old file, then the checkpoint is written.
    Publish,
}
const PHASES: [Phase; 4] = [Phase::Housekeeping, Phase::Prepare, Phase::Stage, Phase::Publish];

/// One extent's bytes on its volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceItem {
    pub extent: Extent,
    pub volume: Volume,
    pub bytes: u64,
    pub phase: Phase,
    /// Exists only during its phase: the compaction pack replaces the files it packs, so after
    /// housekeeping it takes no more room than they did.
    pub transient: bool,
}
impl SpaceItem {
    const fn new(extent: Extent, volume: Volume, bytes: u64, phase: Phase) -> Self {
        Self { extent, volume, bytes, phase, transient: false }
    }
    fn alive(&self, phase: Phase) -> bool {
        if self.transient { self.phase == phase } else { self.phase <= phase }
    }
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
    /// Bytes of private files whose cleanup failed earlier: already on disk (so not charged),
    /// but named in a refusal because restarting NodeCraft frees them.
    cleanup_debt: u64,
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
                SpaceItem::new(Extent::PreparedArchive, Volume::Recovery, archive_bytes, Phase::Prepare),
                SpaceItem::new(Extent::DestinationStage, Volume::Destination, archive_bytes, Phase::Stage),
                SpaceItem::new(Extent::Checkpoint, Volume::Recovery, checkpoint, Phase::Publish),
            ],
            replaced,
            cleanup_debt: 0,
        })
    }
    /// A native open of a file of `file_bytes`: its private frozen copy, then the checkpoint.
    pub fn open(file_bytes: u64) -> Result<Self, Error> {
        let checkpoint = file_bytes.checked_add(CHECKPOINT_HEADER).ok_or_else(overflow)?;
        Ok(Self {
            operation: Operation::Open,
            items: vec![
                SpaceItem::new(Extent::FrozenBase, Volume::Recovery, file_bytes, Phase::Prepare),
                SpaceItem::new(Extent::Checkpoint, Volume::Recovery, checkpoint, Phase::Publish),
            ],
            replaced: None,
            cleanup_debt: 0,
        })
    }
    /// What an open still writes once it has found the file needs rewriting (an older format,
    /// or a legacy identity): the rewritten archive of `archive_bytes` and its checkpoint, on
    /// top of the private copy already made. Add the media the rewrite copies first with
    /// [`Self::with_verified_copies`].
    pub fn rewrite(archive_bytes: u64) -> Result<Self, Error> {
        let checkpoint = archive_bytes.checked_add(CHECKPOINT_HEADER).ok_or_else(overflow)?;
        Ok(Self {
            operation: Operation::Open,
            items: vec![
                SpaceItem::new(Extent::Rewrite, Volume::Recovery, archive_bytes, Phase::Prepare),
                SpaceItem::new(Extent::Checkpoint, Volume::Recovery, checkpoint, Phase::Publish),
            ],
            replaced: None,
            cleanup_debt: 0,
        })
    }
    /// What a classic-format open still writes once its private copy exists: verified copies
    /// of its `copies` bytes of media, then a checkpoint of the `file_bytes` file.
    pub fn verified_open(copies: u64, file_bytes: u64) -> Result<Self, Error> {
        let checkpoint = file_bytes.checked_add(CHECKPOINT_HEADER).ok_or_else(overflow)?;
        Ok(Self {
            operation: Operation::Open,
            items: vec![SpaceItem::new(Extent::Checkpoint, Volume::Recovery, checkpoint, Phase::Publish)],
            replaced: None,
            cleanup_debt: 0,
        }
        .with_verified_copies(copies))
    }
    /// Adds `bytes` of indexed media copied into the workspace before output. They stay: the
    /// archive is written from those copies.
    #[must_use]
    pub fn with_verified_copies(mut self, bytes: u64) -> Self {
        if bytes > 0 {
            self.items.push(SpaceItem::new(Extent::VerifiedCopies, Volume::Recovery, bytes, Phase::Housekeeping));
        }
        self
    }
    /// Adds a compaction pack of `bytes` (every verified backing it packs, new copies included).
    /// It is charged only during housekeeping: once written it replaces the files it packed.
    #[must_use]
    pub fn with_compaction(mut self, bytes: u64) -> Self {
        if bytes > 0 {
            self.items.push(SpaceItem { transient: true, ..SpaceItem::new(Extent::CompactionPack, Volume::Recovery, bytes, Phase::Housekeeping) });
        }
        self
    }
    /// Accounts for replacing a recovery journal of `journal_bytes` with the new checkpoint.
    /// Where the replace briefly keeps both copies of the checkpoint (Windows, measured in CI),
    /// the second copy, less the old journal it frees, is charged at publication. Elsewhere the
    /// replace is an atomic rename and nothing is added.
    #[must_use]
    pub fn with_journal_replaced(mut self, journal_bytes: u64) -> Self {
        let checkpoint = self.items.iter().filter(|item| item.extent == Extent::Checkpoint).map(|item| item.bytes).sum::<u64>();
        let extra = checkpoint.saturating_sub(journal_bytes);
        if cfg!(windows) && extra > 0 {
            self.items.push(SpaceItem::new(Extent::CheckpointReplace, Volume::Recovery, extra, Phase::Publish));
        }
        self
    }
    /// Names `bytes` of earlier failed cleanup in a refusal. It is never charged.
    #[must_use]
    pub fn with_cleanup_debt(mut self, bytes: u64) -> Self {
        self.cleanup_debt = bytes;
        self
    }
    pub fn items(&self) -> &[SpaceItem] {
        &self.items
    }
    /// Bytes needed on `volume` at its peak, slack included. On separate volumes each one is
    /// charged what it holds at its own peak: freeing the replaced file only shrinks the
    /// destination's share after its peak.
    pub fn needed(&self, volume: Volume) -> Result<u64, Error> {
        self.peak(|item| item.volume == volume, |_| 0)
    }
    /// The peak, over phases, of the extents `counted` alive then, less `credit` (space freed
    /// by that phase). A credit never makes a phase negative.
    fn peak(&self, counted: impl Fn(&SpaceItem) -> bool, credit: impl Fn(Phase) -> u64) -> Result<u64, Error> {
        PHASES.into_iter().try_fold(0u64, |peak, phase| {
            let alive = self
                .items
                .iter()
                .filter(|item| counted(item) && item.alive(phase))
                .try_fold(0u64, |total, item| total.checked_add(item.bytes).and_then(|n| n.checked_add(EXTENT_SLACK)).ok_or_else(overflow))?;
            Ok(peak.max(alive.saturating_sub(credit(phase))))
        })
    }
    /// The peak when every extent shares one volume. Before publication the prepared archive and
    /// the staged file coexist with the old file; after it the old file is gone and the
    /// checkpoint is written beside the other two.
    pub fn needed_on_one_volume(&self) -> Result<u64, Error> {
        let replaced = self.replaced.unwrap_or(0);
        self.peak(|_| true, |phase| if phase == Phase::Publish { replaced } else { 0 })
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
        // Debt sits in the recovery folder, so it matters only where that drive is short.
        let debt = if self.cleanup_debt > 0 && !matches!(at, Where::Destination) {
            format!(
                " NodeCraft is also holding {} of temporary files it couldn't delete there; closing and reopening NodeCraft frees them.",
                size(self.cleanup_debt)
            )
        } else {
            String::new()
        };
        // Sizes, never paths: the message may be shown, logged or sent to an agent.
        Error::new(
            ErrorKind::Busy,
            format!("Not enough free disk space to {what}: it needs about {} on {drive}, and {} is free. {fix} {unchanged}{debt}", size(needed), size(free)),
        )
    }
}
/// The check an open makes once the file's private copy is made and its index read (#367).
/// A classic-format file (every project small enough for the classic wire) has each of its
/// assets copied and verified in the workspace before it is decoded, then a checkpoint of the
/// copy written; neither is known before the file is read. Checked against the space left now.
pub fn check_open_copies(
    index: &crate::zip_index::ArchiveIndex,
    context: &crate::AssetContext,
    capability: crate::StorageCapability,
    probe: &SpaceProbe,
) -> Result<(), Error> {
    if capability != crate::StorageCapability::StreamingV2 || probe.recovery_free.is_none() || !crate::archive::is_classic_wire(index.version()) {
        return Ok(());
    }
    let assets: Vec<_> = index
        .entries()
        .iter()
        .filter_map(|entry| match entry.name {
            crate::zip_index::EntryName::Asset(hash) => Some((hash, entry.data.byte_len())),
            crate::zip_index::EntryName::Manifest => None,
        })
        .collect();
    // An asset this workspace already verified (the same media in the open project) is reused,
    // not copied. Another verification running means it can't be told: no refusal then.
    let hashes: Vec<_> = assets.iter().map(|(hash, _)| *hash).collect();
    let Some(verified) = context.verified_among(&hashes)? else { return Ok(()) };
    let copies = assets
        .iter()
        .zip(verified)
        .filter(|(_, verified)| !verified)
        .try_fold(0u64, |total, ((_, bytes), _)| total.checked_add(*bytes).ok_or_else(overflow))?;
    SpaceInventory::verified_open(copies, index.identity().archive_bytes)?.with_cleanup_debt(context.cleanup_debt()?).check(probe)
}
/// The check an open makes once it has found that the file needs rewriting (#367): the
/// rewritten archive of `snapshot` (estimated without reading its media), the media copies it
/// is written from, any compaction pack and its checkpoint, against the space `probe` reports
/// now, after the private copy was made. Unknown free space or an estimate that can't be made
/// let the open go ahead; the real writes then decide.
pub fn check_rewrite(snapshot: &crate::Snapshot, capability: crate::StorageCapability, probe: &SpaceProbe) -> Result<(), Error> {
    if capability != crate::StorageCapability::StreamingV2 || probe.recovery_free.is_none() {
        return Ok(());
    }
    let Ok(bytes) = crate::ArchivePlan::estimate_bytes(&snapshot.project, &snapshot.assets, snapshot.epoch, capability) else { return Ok(()) };
    let output = snapshot.assets.output_space(&snapshot.project, &std::collections::BTreeSet::new()).ok().flatten().unwrap_or_default();
    let mut inventory = SpaceInventory::rewrite(bytes)?.with_verified_copies(output.verified_copies).with_cleanup_debt(output.cleanup_debt);
    if let Some(pack) = output.compaction {
        inventory = inventory.with_compaction(pack);
    }
    inventory.check(probe)
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

    #[test]
    fn housekeeping_copies_stay_but_the_compaction_pack_only_peaks_while_it_is_written() {
        // A save right after opening: every asset is copied and verified before the archive.
        let save = SpaceInventory::save(GB, Some(GB)).unwrap().with_verified_copies(GB);
        // Recovery holds the copies, the archive and the checkpoint at its peak.
        assert_eq!(save.needed(Volume::Recovery).unwrap(), 3 * GB + 80 + 3 * EXTENT_SLACK);
        assert_eq!(save.needed(Volume::Destination).unwrap(), GB + EXTENT_SLACK);
        // One volume: copies + archive + stage before publication; after it the old file is
        // freed before the checkpoint, which is the (slightly larger) peak.
        assert_eq!(save.needed_on_one_volume().unwrap(), 3 * GB + 80 + 4 * EXTENT_SLACK);
        // A pack of the copies (and 0.5 GB already cached) doubles them only while it's written,
        // which here is below the later peak: it changes nothing.
        let packed = save.clone().with_compaction(GB + GB / 2);
        assert_eq!(packed.needed_on_one_volume().unwrap(), save.needed_on_one_volume().unwrap());
        // A pack larger than everything after it is the peak, and only then.
        let small = SpaceInventory::save(GB / 10, None).unwrap().with_compaction(5 * GB);
        assert_eq!(small.needed(Volume::Recovery).unwrap(), 5 * GB + EXTENT_SLACK);
        // Zero-byte additions add no slack either.
        let plain = SpaceInventory::save(GB, None).unwrap();
        assert_eq!(plain.clone().with_verified_copies(0).with_compaction(0), plain);
    }

    #[test]
    fn a_resave_after_open_with_every_asset_unverified_fits_where_it_really_fits() {
        // 10 GB project just opened: its 10 GB of media are copied (base stays), then a 10 GB
        // archive and a 10 GB staged file; publication frees the old 10 GB before the checkpoint.
        let resave = SpaceInventory::save(10 * GB, Some(10 * GB)).unwrap().with_verified_copies(10 * GB);
        let peak = 30 * GB + 80 + 4 * EXTENT_SLACK;
        assert_eq!(resave.needed_on_one_volume().unwrap(), peak);
        let probe = |free| SpaceProbe { destination_free: Some(free), recovery_free: Some(free), shared: true, existing_bytes: Some(10 * GB) };
        assert!(resave.check(&probe(peak)).is_ok());
        assert!(resave.check(&probe(peak - 1)).is_err());
    }

    #[test]
    fn an_open_rewrite_is_charged_on_the_recovery_drive_only() {
        let rewrite = SpaceInventory::rewrite(GB).unwrap().with_verified_copies(GB);
        assert_eq!(rewrite.needed(Volume::Destination).unwrap(), 0);
        assert_eq!(rewrite.needed(Volume::Recovery).unwrap(), 3 * GB + 80 + 3 * EXTENT_SLACK);
        let probe = SpaceProbe { destination_free: Some(1), recovery_free: Some(2 * GB), shared: true, existing_bytes: Some(GB) };
        let error = rewrite.check(&probe).unwrap_err();
        assert!(error.message.contains("open this project") && error.message.contains("current project is unchanged"), "{}", error.message);
    }

    #[test]
    fn cleanup_debt_is_named_where_it_lives_and_never_charged() {
        let save = SpaceInventory::save(GB, None).unwrap();
        let indebted = save.clone().with_cleanup_debt(3 * GB);
        assert_eq!(indebted.needed_on_one_volume().unwrap(), save.needed_on_one_volume().unwrap());
        let shared = SpaceProbe { destination_free: Some(GB), recovery_free: Some(GB), shared: true, existing_bytes: None };
        let error = indebted.check(&shared).unwrap_err();
        assert!(error.message.contains("3.0 GB of temporary files") && error.message.contains("reopening NodeCraft"), "{}", error.message);
        assert!(!save.check(&shared).unwrap_err().message.contains("temporary files"));
        // A short destination drive isn't helped by recovery-folder debt.
        let destination = SpaceProbe { destination_free: Some(1), recovery_free: None, shared: false, existing_bytes: None };
        assert!(!indebted.check(&destination).unwrap_err().message.contains("temporary files"));
        assert!(SpaceInventory::save(GB, None).unwrap().with_verified_copies(u64::MAX).needed(Volume::Recovery).is_err());
    }

    #[test]
    fn replacing_the_journal_doubles_the_checkpoint_only_where_the_replace_keeps_both() {
        let save = SpaceInventory::save(GB, None).unwrap();
        let replaced = save.clone().with_journal_replaced(GB / 4);
        let extra = if cfg!(windows) { 3 * GB / 4 + 80 + EXTENT_SLACK } else { 0 };
        assert_eq!(replaced.needed_on_one_volume().unwrap(), save.needed_on_one_volume().unwrap() + extra);
        // A journal at least as big as the new checkpoint frees all of the second copy's room.
        assert_eq!(save.clone().with_journal_replaced(2 * GB), save);
    }
}
