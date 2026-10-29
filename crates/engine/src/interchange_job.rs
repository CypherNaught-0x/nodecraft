//! Small owned export input: no project/history clone and no asset-body read on dispatch.
//! The worker builds bounded XML, then streams a single privately verified media lease at a
//! time. Dropping a document cannot invalidate these saved descriptors or source ownership.
use nodecraft_format::{
    AssetStore,
    interchange::{Bundle, MAX_MEDIA, MAX_MEDIA_BYTES},
};
use nodecraft_nodes::timeline::{
    Timeline, Wire,
    interchange::{self, Interchange, Sequence, Source},
};
use nodecraft_types::{AssetRef, Error, ErrorKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub(crate) name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: u32,
    pub(crate) background: [u8; 3],
    pub(crate) timeline: Timeline,
    pub(crate) wires: Vec<Wire>,
    pub(crate) media: Vec<Option<usize>>,
    pub(crate) sources: Vec<(Source, AssetRef)>,
}

pub(crate) struct Prepared {
    pub(crate) name: String,
    pub(crate) fps: u32,
    pub(crate) interchange: Interchange,
    pub(crate) sources: Vec<(Source, AssetRef)>,
}
impl Recipe {
    /// Resolve only the bounded clip metadata; do not produce XML or open any body here.
    pub(crate) fn check(&self, assets: &AssetStore) -> Result<(), Error> {
        let mut used = std::collections::BTreeSet::new();
        let mut bytes = 0u64;
        let listed = self.timeline.with_sources(&self.wires);
        for (n, (input, clip)) in self.timeline.resolve(&self.wires).into_iter().enumerate() {
            let number = listed.clips.iter().position(|c| c.id == clip.id).unwrap_or(n).saturating_add(1);
            let index = self
                .media
                .get(input)
                .copied()
                .flatten()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("clip {number} has no media yet; run its source first")))?;
            if used.insert(index) {
                if used.len() > MAX_MEDIA {
                    return Err(Error::new(ErrorKind::InvalidInput, "the timeline names too many media files to export"));
                }
                let (_, asset) = self.sources.get(index).ok_or_else(|| Error::new(ErrorKind::Internal, "the timeline's source isn't listed"))?;
                let descriptor = assets.descriptor(&asset.hash).ok_or_else(|| Error::new(ErrorKind::NotFound, "the timeline references missing media"))?;
                bytes = bytes
                    .checked_add(descriptor.bytes)
                    .filter(|bytes| *bytes <= MAX_MEDIA_BYTES)
                    .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the timeline's media exceeds the 8 GiB export limit"))?;
            }
            // All times/placement were checked by Timeline::parse. Explicit ends can be
            // checked without a read; unknown media duration is resolved by the pure writer.
            if clip.out_us.is_some_and(|end| end <= clip.in_us) {
                return Err(Error::new(ErrorKind::InvalidInput, "a timeline clip's trim is empty"));
            }
        }
        if used.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "there is nothing to export"));
        }
        Ok(())
    }

    pub(crate) fn prepare(&self, check: &mut dyn FnMut() -> Result<(), Error>) -> Result<Prepared, Error> {
        check()?;
        let listed: Vec<Source> = self.sources.iter().map(|(source, _)| source.clone()).collect();
        let sequence = Sequence {
            name: &self.name,
            width: self.width,
            height: self.height,
            fps: self.fps,
            background: self.background,
            timeline: &self.timeline,
            wires: &self.wires,
            media: &self.media,
            sources: &listed,
        };
        let interchange = interchange::fcpxml_controlled(&sequence, check)?;
        Ok(Prepared { name: self.name.clone(), fps: self.fps, interchange, sources: self.sources.clone() })
    }
}
impl Prepared {
    pub(crate) fn bundle(&self) -> Result<Bundle, Error> {
        let mut media = Vec::with_capacity(self.interchange.used.len());
        for index in &self.interchange.used {
            let (source, asset) = self.sources.get(*index).ok_or_else(|| Error::new(ErrorKind::Internal, "the document names media that isn't listed"))?;
            media.push((source.file.clone(), asset.clone()));
        }
        let stem = crate::interchange_cmds::stem(&self.name, "Timeline");
        let bundle = Bundle {
            document: format!("{stem}.fcpxml"),
            xml: self.interchange.xml.clone(),
            report: crate::interchange_cmds::report(self),
            subtitles: self.interchange.srt.as_ref().map(|srt| (format!("{stem}.srt"), srt.clone())),
            media,
        };
        bundle.check()?;
        Ok(bundle)
    }
}
