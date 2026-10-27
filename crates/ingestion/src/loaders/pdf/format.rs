//! Shared output formatter for PDF backends.
//!
//! Both the pdfium and pure-Rust backends call [`format_pages`], so the
//! `"Page N:"` envelope they produce is byte-identical and matches the Python
//! `pypdf_loader.py:70-84` format. The page text each hands *in* is not — see
//! the [`super`] module docs for the measured difference.

use crate::loaders::LoaderError;

/// The start of every zero-page error, whichever backend raised it.
pub const NO_PAGES: &str = "PDF parsed but no pages could be read";

/// [`format_pages`] for a whole document, with the rule both backends share:
/// a document with no pages is an error, not an empty text.
///
/// A PDF has at least one page. A parse that yields none means the page tree
/// is empty or unreadable, and storing that as an empty document would make
/// `add` report success for a file nothing can find. `no_pages_detail` is the
/// backend's explanation of why, appended to [`NO_PAGES`]; it is only called
/// on that path. Pages that exist but carry no text (scanned, image-only,
/// blank) are not affected and still format to `""`.
pub fn format_document(
    pages: &[(usize, Result<String, String>)],
    no_pages_detail: impl FnOnce() -> String,
) -> Result<String, LoaderError> {
    if pages.is_empty() {
        return Err(LoaderError::ExtractionFailed(format!(
            "{NO_PAGES}{}",
            no_pages_detail()
        )));
    }
    Ok(format_pages(pages))
}

/// Formats per-page text into the Python-compatible output format.
///
/// Pages are 1-indexed. Empty pages and failed pages are skipped.
/// Per-page errors are logged at `warn!` level.
///
/// Output: `"Page 1:\n{text}\n\nPage 2:\n{text}\n\n..."`
pub fn format_pages(pages: &[(usize, Result<String, String>)]) -> String {
    let mut parts = Vec::new();
    for (page_num, result) in pages {
        match result {
            Ok(text) if !text.trim().is_empty() => {
                parts.push(format!("Page {page_num}:\n{text}\n"));
            }
            Ok(_) => {
                // Empty page -- skip silently
            }
            Err(e) => {
                tracing::warn!(page = page_num, error = %e, "Failed to extract text from page");
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn single_page() {
        let pages = vec![(1, Ok("Hello world".to_string()))];
        assert_eq!(format_pages(&pages), "Page 1:\nHello world\n");
    }

    #[test]
    fn multiple_pages() {
        let pages = vec![
            (1, Ok("First page".to_string())),
            (2, Ok("Second page".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nFirst page\n\nPage 2:\nSecond page\n"
        );
    }

    #[test]
    fn empty_pages_skipped() {
        let pages = vec![
            (1, Ok("Content".to_string())),
            (2, Ok("".to_string())),
            (3, Ok("   \n  ".to_string())),
            (4, Ok("More content".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nContent\n\nPage 4:\nMore content\n"
        );
    }

    #[test]
    fn error_pages_skipped() {
        let pages = vec![
            (1, Ok("Content".to_string())),
            (2, Err("decode error".to_string())),
            (3, Ok("After error".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nContent\n\nPage 3:\nAfter error\n"
        );
    }

    #[test]
    fn all_empty_returns_empty_string() {
        let pages: Vec<(usize, Result<String, String>)> =
            vec![(1, Ok("".to_string())), (2, Ok("  ".to_string()))];
        assert_eq!(format_pages(&pages), "");
    }

    #[test]
    fn no_pages_returns_empty_string() {
        let pages: Vec<(usize, Result<String, String>)> = vec![];
        assert_eq!(format_pages(&pages), "");
    }

    #[test]
    fn a_document_with_no_pages_is_an_error() {
        let err = format_document(&[], || " (detail).".to_string()).unwrap_err();
        assert!(matches!(err, LoaderError::ExtractionFailed(_)), "{err}");
        assert!(
            err.to_string().ends_with(&format!("{NO_PAGES} (detail).")),
            "{err}"
        );
    }

    #[test]
    fn a_document_whose_pages_have_no_text_formats_to_empty() {
        let pages = vec![(1, Ok(" ".to_string())), (2, Err("bad page".to_string()))];
        let text = format_document(&pages, || unreachable!("the document has pages")).unwrap();
        assert_eq!(text, "");
    }

    #[test]
    fn page_numbering_preserved_with_gaps() {
        // Pages 1, 3, 5 -- numbering reflects original document position
        let pages = vec![
            (1, Ok("First".to_string())),
            (3, Ok("Third".to_string())),
            (5, Ok("Fifth".to_string())),
        ];
        assert_eq!(
            format_pages(&pages),
            "Page 1:\nFirst\n\nPage 3:\nThird\n\nPage 5:\nFifth\n"
        );
    }
}
