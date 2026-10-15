use std::sync::Arc;

use async_trait::async_trait;
use cognee_session::SessionContext;

use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};

pub type SearchRetrieverRef = Arc<dyn SearchRetriever>;

/// The [`SearchTuning`](crate::orchestration::SearchTuning) knobs a retriever
/// was built with, for tests that check `SearchBuilder` wires the tuning into
/// every retriever that consumes it. `None` means "this retriever has no such
/// knob".
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppliedTuning {
    /// `search.triplet_neighbor_cap` (itself optional: `None` is uncapped).
    pub neighbor_cap: Option<Option<usize>>,
    /// `search.allow_cypher_query`.
    pub allow_cypher_query: Option<bool>,
    /// The `search.hybrid` budget.
    pub hybrid: Option<super::HybridBudget>,
}

/// How a retriever's results can be restricted to a set of datasets.
///
/// Every dataset shares one graph and one vector store here, so a dataset
/// scope is applied by filtering what a retriever returns on each item's
/// dataset membership (`dataset_ids` / `dataset_id` in the payload; see
/// [`crate::orchestration::filter_context_to_datasets`]). Python instead runs
/// the retriever once per dataset inside that dataset's own database context
/// (`set_database_global_context_variables`, `search.py:330` at f154b32), which
/// scopes every search type. A retriever whose items carry no membership
/// cannot be scoped by filtering: every item would be dropped (an empty
/// answer) or, unfiltered, every dataset would be read. It must say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetScoping {
    /// Every context item carries its dataset membership, and the completion
    /// reads only the context it is handed, plus follow-up retrievals that
    /// apply `SearchParams::dataset_ids` themselves. The orchestrator's
    /// membership filter scopes it.
    Membership,
    /// The results are not dataset data (the feedback analysis of the user's
    /// own message), so a dataset scope does not apply to them.
    NotDatasetData,
    /// The results cannot be attributed to datasets. With access control
    /// wired, a dataset-scoped search of this type is refused rather than
    /// answered empty or from datasets the caller may not read.
    Unsupported,
}

#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    /// How this retriever's results can be restricted to datasets. Defaults
    /// to [`DatasetScoping::Unsupported`], the safe answer for a retriever
    /// (a community one, say) that has not been checked.
    fn dataset_scoping(&self) -> DatasetScoping {
        DatasetScoping::Unsupported
    }

    /// The tuning knobs this retriever was built with (test-only
    /// introspection; see [`AppliedTuning`]).
    #[cfg(test)]
    fn applied_tuning(&self) -> AppliedTuning {
        AppliedTuning::default()
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError>;

    /// Process multiple queries in sequence and return their contexts.
    ///
    /// Default: loops over [`get_context`]. Override for efficient batching.
    async fn get_context_batch(
        &self,
        queries: &[String],
        params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        let mut results = Vec::with_capacity(queries.len());
        for query in queries {
            results.push(self.get_context(query, params).await?);
        }
        Ok(results)
    }

    /// Process multiple queries and return their completions.
    ///
    /// Default: loops over [`get_completion`]. Override for efficient batching.
    async fn get_completion_batch(
        &self,
        queries: &[String],
        contexts: Option<Vec<SearchContext>>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Vec<SearchOutput>, SearchError> {
        let mut results = Vec::with_capacity(queries.len());
        for (i, query) in queries.iter().enumerate() {
            let ctx = contexts.as_ref().and_then(|cs| cs.get(i).cloned());
            results.push(self.get_completion(query, ctx, session, params).await?);
        }
        Ok(results)
    }
}
