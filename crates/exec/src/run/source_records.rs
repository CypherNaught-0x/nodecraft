//! Optional Timeline bookkeeping is bounded before retention and cannot discard a render.
use nodecraft_graph::{MAX_SOURCE_VERSIONS, SourceVersion};
use nodecraft_types::Error;

pub(super) const OMITTED: &str = "Timeline source versions could not be recorded; the rendered output was kept without source provenance";

pub(super) struct SourceRecords {
    cells: Vec<Vec<SourceVersion>>,
    count: Option<usize>,
}

impl SourceRecords {
    pub(super) fn new(cells: usize) -> Self {
        Self { cells: vec![Vec::new(); cells], count: Some(0) }
    }

    pub(super) fn available(&self) -> bool {
        self.count.is_some()
    }

    pub(super) fn clear(&mut self, cell: usize) {
        if let Some(sources) = self.cells.get_mut(cell) {
            self.count = self.count.map(|count| count.saturating_sub(sources.len()));
            sources.clear();
        }
    }

    pub(super) fn append(&mut self, cell: usize, sources: Result<Vec<SourceVersion>, Error>) {
        let Some(count) = self.count else { return };
        let Ok(sources) = sources else { return self.omit() };
        let Some(count) = count.checked_add(sources.len()).filter(|count| *count <= MAX_SOURCE_VERSIONS) else { return self.omit() };
        let Some(target) = self.cells.get_mut(cell) else { return self.omit() };
        target.extend(sources);
        self.count = Some(count);
    }

    fn omit(&mut self) {
        self.count = None;
        self.cells.iter_mut().for_each(Vec::clear);
        // Fixed text only: recipes and their errors may contain private input values.
        log::warn!("{OMITTED}");
    }

    pub(super) fn finish(self) -> Vec<SourceVersion> {
        self.cells.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{Blake3, ErrorKind};

    fn source(n: usize) -> SourceVersion {
        SourceVersion { role: format!("clip{n}"), hash: Blake3::from_bytes([1; 32]), item: None }
    }

    #[test]
    fn a_failed_recipe_discards_partial_records_and_cannot_be_resumed() {
        let mut records = SourceRecords::new(2);
        records.append(0, Ok(vec![source(0)]));
        records.append(1, Err(Error::new(ErrorKind::InvalidInput, "private recipe text")));
        assert!(!records.available());
        records.append(1, Ok(vec![source(1)]));
        assert!(records.finish().is_empty());
    }

    #[test]
    fn the_record_cap_is_shared_by_cells_and_checked_before_extending() {
        let mut records = SourceRecords::new(2);
        records.append(0, Ok((0..MAX_SOURCE_VERSIONS).map(source).collect()));
        assert!(records.available());
        assert_eq!(records.cells.iter().map(Vec::len).sum::<usize>(), MAX_SOURCE_VERSIONS);
        records.append(1, Ok(vec![source(MAX_SOURCE_VERSIONS)]));
        assert!(!records.available());
        assert!(records.finish().is_empty());
    }

    #[test]
    fn clearing_a_missing_cell_returns_its_record_budget() {
        let mut records = SourceRecords::new(2);
        records.append(0, Ok((0..MAX_SOURCE_VERSIONS).map(source).collect()));
        records.clear(0);
        records.append(1, Ok(vec![source(1)]));
        assert_eq!(records.finish(), vec![source(1)]);
    }
}
