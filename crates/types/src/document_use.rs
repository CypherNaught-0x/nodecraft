//! How a generation mode reads a wired PDF (#591): the most pages one request sends and the
//! prompt tokens a provider counts per page, so a run is refused before it is sent and its input
//! cost is estimated before it runs. Catalogs declare it per mode; planning and quoting read it.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};

/// Most pages a catalog may let one request send.
pub const MAX_DOCUMENT_PAGES: u32 = 1000;
/// Most prompt tokens a catalog may count per page.
pub const MAX_TOKENS_PER_PAGE: u32 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentUse {
    /// The mode's document input role this describes.
    pub role: String,
    /// Most pages one request sends, across every PDF wired to the role.
    pub max_pages: u32,
    /// The prompt tokens the provider counts for one page (its text and its picture).
    pub tokens_per_page: u32,
}

impl DocumentUse {
    pub fn validate(&self) -> Result<(), Error> {
        if self.max_pages == 0 || self.max_pages > MAX_DOCUMENT_PAGES {
            return Err(Error::new(ErrorKind::InvalidInput, format!("`max_pages` is 1 to {MAX_DOCUMENT_PAGES}")));
        }
        if self.tokens_per_page == 0 || self.tokens_per_page > MAX_TOKENS_PER_PAGE {
            return Err(Error::new(ErrorKind::InvalidInput, format!("`tokens_per_page` is 1 to {MAX_TOKENS_PER_PAGE}")));
        }
        Ok(())
    }

    /// The counted pages one request sends, from each wired PDF's page count; refused when they
    /// are more than the mode takes. A PDF nobody counted (one an action made) goes through and the
    /// provider still checks it, as an unmeasured source video does (#179).
    pub fn pages(&self, counts: impl IntoIterator<Item = Option<u32>>) -> Result<u32, Error> {
        let total = counts.into_iter().flatten().fold(0u32, u32::saturating_add);
        if total > self.max_pages {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "the {} PDFs have {total} pages and this model reads at most {} at once; send fewer pages, or use PDF to text",
                    self.role, self.max_pages
                ),
            ));
        }
        Ok(total)
    }

    /// The pages a quote counts for the wired PDFs: an uncounted PDF (one an earlier card makes, or
    /// one nobody counted) counts as the most the mode reads, and the total never exceeds that, so
    /// the quote stays an upper bound whatever reaches the provider.
    pub fn quoted_pages(&self, counts: impl IntoIterator<Item = Option<u32>>) -> u32 {
        counts.into_iter().map(|count| count.unwrap_or(self.max_pages)).fold(0u32, u32::saturating_add).min(self.max_pages)
    }

    /// The prompt tokens `pages` pages count as.
    pub fn tokens(&self, pages: u32) -> u64 {
        u64::from(pages).saturating_mul(u64::from(self.tokens_per_page))
    }

    /// One line a person reads in the model picker.
    pub fn summary(&self) -> String {
        format!("{}: PDFs up to {} pages, about {} tokens a page", self.role, self.max_pages, self.tokens_per_page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf() -> DocumentUse {
        DocumentUse { role: "document".into(), max_pages: 100, tokens_per_page: 2000 }
    }

    #[test]
    fn pages_are_counted_and_bounded() {
        assert_eq!(pdf().pages([Some(40), Some(60)]).unwrap(), 100);
        assert_eq!(pdf().pages([]).unwrap(), 0);
        let over = pdf().pages([Some(60), Some(41)]).unwrap_err();
        assert!(over.message.contains("101 pages") && over.message.contains("PDF to text"), "{}", over.message);
        assert_eq!(pdf().pages([Some(1), None]).unwrap(), 1, "an uncounted PDF goes through");
        assert!(pdf().pages([Some(101), None]).is_err());
        assert!(pdf().pages([Some(u32::MAX), Some(u32::MAX)]).is_err(), "hostile counts don't overflow");
        assert_eq!(pdf().tokens(u32::MAX), u64::from(u32::MAX) * 2000);
        assert_eq!(pdf().quoted_pages([Some(12), Some(30)]), 42);
        assert_eq!(pdf().quoted_pages([Some(12), None]), 100, "an uncounted PDF is quoted at the limit");
        assert_eq!(pdf().quoted_pages([Some(u32::MAX), Some(7)]), 100);
        assert_eq!(pdf().quoted_pages([]), 0);
    }

    #[test]
    fn declarations_are_checked() {
        assert!(pdf().validate().is_ok());
        assert!(DocumentUse { max_pages: 0, ..pdf() }.validate().is_err());
        assert!(DocumentUse { max_pages: MAX_DOCUMENT_PAGES + 1, ..pdf() }.validate().is_err());
        assert!(DocumentUse { tokens_per_page: 0, ..pdf() }.validate().is_err());
        assert_eq!(pdf().summary(), "document: PDFs up to 100 pages, about 2000 tokens a page");
    }
}
