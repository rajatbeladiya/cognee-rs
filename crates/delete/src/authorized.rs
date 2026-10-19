//! ACL-enforcing wrapper around [`DeleteService`].
//!
//! [`AuthorizedDeleteService`] checks that the calling principal holds the
//! `"delete"` permission on each target dataset before delegating to the
//! inner [`DeleteService`]. The plain `DeleteService` remains available for
//! edge/embedded deployments that do not require ACL enforcement.
//!
//! Every check is **tenant-scoped**: a grant counts only on a dataset whose
//! `tenant_id` equals the caller's tenant (strict equality, `None` admits only
//! tenant-less datasets), exactly like Python's
//! `get_all_user_permission_datasets` filter
//! (`cognee/modules/users/permissions/methods/get_all_user_permission_datasets.py:49-53@37f788bd2`).
//! A grant only in another tenant is denied with the same
//! [`DeleteError::PermissionDenied`] as no grant at all.

use std::collections::HashSet;
use std::sync::Arc;

use cognee_database::{AclDb, DeleteDb};
use uuid::Uuid;

use crate::{DeleteError, DeletePreview, DeleteRequest, DeleteResult, DeleteScope, DeleteService};

/// Authorization-enforcing delete service.
///
/// Wraps a [`DeleteService`] and an [`AclDb`] implementation. Before every
/// `execute()` or `preview()` call the wrapper verifies that `principal_id`,
/// acting in `tenant_id`, holds `"delete"` permission on all affected
/// datasets (see the module docs for the tenant rule).
pub struct AuthorizedDeleteService {
    inner: DeleteService,
    acl_db: Arc<dyn AclDb>,
    database: Arc<dyn DeleteDb>,
}

impl AuthorizedDeleteService {
    /// Create a new authorized delete service.
    ///
    /// `database` must be the same connection used to construct the inner
    /// `DeleteService`, so that dataset resolution is consistent.
    pub fn new(inner: DeleteService, acl_db: Arc<dyn AclDb>, database: Arc<dyn DeleteDb>) -> Self {
        Self {
            inner,
            acl_db,
            database,
        }
    }

    /// Preview the deletion, checking ACL first.
    ///
    /// [`DeleteScope::All`] is previewed as the sum over the caller's
    /// `delete`-authorized datasets in `tenant_id` (see [`Self::execute`]).
    pub async fn preview(
        &self,
        request: &DeleteRequest,
        principal_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<DeletePreview, DeleteError> {
        if matches!(request.scope, DeleteScope::All) {
            let mut total = DeletePreview::default();
            for dataset_id in self.deletable_in_tenant(principal_id, tenant_id).await? {
                let p = self.inner.preview(&by_id(request, dataset_id)).await?;
                total.datasets_to_delete += p.datasets_to_delete;
                total.dataset_links_to_delete += p.dataset_links_to_delete;
                total.data_to_delete += p.data_to_delete;
                total.storage_files_to_delete += p.storage_files_to_delete;
                total.graph_nodes_to_delete += p.graph_nodes_to_delete;
                total.vector_points_to_delete += p.vector_points_to_delete;
                total.provenance_nodes_to_delete += p.provenance_nodes_to_delete;
                total.provenance_edges_to_delete += p.provenance_edges_to_delete;
                total.search_queries_to_delete += p.search_queries_to_delete;
                total.orphaned_edge_types_to_delete += p.orphaned_edge_types_to_delete;
            }
            return Ok(total);
        }
        self.check_authorization(request, principal_id, tenant_id)
            .await?;
        self.inner.preview(request).await
    }

    /// Execute the deletion, checking ACL first.
    ///
    /// [`DeleteScope::All`] is **not** a database-wide wipe here: like Python's
    /// `datasets.delete_all`, which enumerates
    /// `get_authorized_existing_datasets([], "delete", user)` and empties each
    /// (`cognee/api/v1/datasets/datasets.py:316-323@37f788bd2`), it deletes —
    /// by id — every dataset the caller holds `delete` on in `tenant_id`, and
    /// leaves everything else (other tenants, ungranted datasets) alone. It
    /// therefore cannot be denied by a dataset the caller cannot see. The
    /// system-wide artifacts of the plain [`DeleteService`]'s `All` (whole
    /// graph/vector wipe, all search history) are not touched.
    pub async fn execute(
        &self,
        request: &DeleteRequest,
        principal_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<DeleteResult, DeleteError> {
        if matches!(request.scope, DeleteScope::All) {
            let mut total = DeleteResult::default();
            for dataset_id in self.deletable_in_tenant(principal_id, tenant_id).await? {
                let r = self.inner.execute(&by_id(request, dataset_id)).await?;
                total.deleted_datasets += r.deleted_datasets;
                total.deleted_dataset_links += r.deleted_dataset_links;
                total.deleted_data += r.deleted_data;
                total.deleted_storage_files += r.deleted_storage_files;
                total.deleted_graph_nodes += r.deleted_graph_nodes;
                total.deleted_vector_points += r.deleted_vector_points;
                total.deleted_provenance_nodes += r.deleted_provenance_nodes;
                total.deleted_provenance_edges += r.deleted_provenance_edges;
                total.deleted_orphan_entities += r.deleted_orphan_entities;
                total.deleted_orphan_entity_types += r.deleted_orphan_entity_types;
                total.deleted_orphan_edge_types += r.deleted_orphan_edge_types;
                total.deleted_pipeline_runs += r.deleted_pipeline_runs;
                total.cleared_pipeline_statuses += r.cleared_pipeline_statuses;
                total.deleted_search_queries += r.deleted_search_queries;
                total.pruned_sessions |= r.pruned_sessions;
                total.warnings.extend(r.warnings);
            }
            return Ok(total);
        }
        self.check_authorization(request, principal_id, tenant_id)
            .await?;
        self.inner.execute(request).await
    }

    /// Existing datasets the principal holds `delete` on in `tenant_id`
    /// (strict tenant equality), in listing order. Grants on ids with no
    /// dataset row are dropped.
    async fn deletable_in_tenant(
        &self,
        principal_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Vec<Uuid>, DeleteError> {
        let authorized: HashSet<Uuid> = self
            .acl_db
            .authorized_dataset_ids_in_tenant(principal_id, "delete", tenant_id)
            .await
            .map_err(|e| DeleteError::Runtime(format!("ACL query failed: {e}")))?
            .into_iter()
            .collect();
        if authorized.is_empty() {
            return Ok(Vec::new());
        }
        let datasets = self
            .database
            .list_datasets()
            .await
            .map_err(|e| DeleteError::Runtime(format!("Failed to list all datasets: {e}")))?;
        Ok(datasets
            .into_iter()
            .filter(|ds| authorized.contains(&ds.id))
            .map(|ds| ds.id)
            .collect())
    }

    /// Verify that `principal_id` has "delete" permission on all datasets
    /// that would be affected by the request, in `tenant_id`.
    async fn check_authorization(
        &self,
        request: &DeleteRequest,
        principal_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<(), DeleteError> {
        match &request.scope {
            DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name,
                ..
            } => {
                if let Some(ds_name) = dataset_name {
                    let dataset_id = self.resolve_dataset_id(*owner_id, ds_name).await?;
                    self.require_delete_permission(principal_id, tenant_id, dataset_id)
                        .await?;
                } else {
                    // When no dataset_name is given, the inner service resolves
                    // all datasets linked to this data item that belong to the
                    // owner. We must check that the principal has delete
                    // permission on each of those datasets.
                    let datasets = self
                        .database
                        .list_datasets_for_data(*data_id)
                        .await
                        .map_err(|e| {
                            DeleteError::Runtime(format!(
                                "Failed to list datasets for data {data_id}: {e}"
                            ))
                        })?;

                    for ds in &datasets {
                        if ds.owner_id == *owner_id {
                            self.require_delete_permission(principal_id, tenant_id, ds.id)
                                .await?;
                        }
                    }
                }
            }
            DeleteScope::Dataset {
                owner_id,
                dataset_name,
            } => {
                let dataset_id = self.resolve_dataset_id(*owner_id, dataset_name).await?;
                self.require_delete_permission(principal_id, tenant_id, dataset_id)
                    .await?;
            }
            // By id: Python's `get_authorized_dataset(user, dataset_id,
            // "delete")` — a `delete` grant on that dataset, not ownership
            // (SDK-936).
            DeleteScope::DataInDataset { dataset_id, .. }
            | DeleteScope::DatasetById { dataset_id } => {
                self.require_delete_permission(principal_id, tenant_id, *dataset_id)
                    .await?;
            }
            DeleteScope::User { owner_id } => {
                // For user-scoped deletion, verify the principal has delete
                // on all datasets that would be affected. The inner service
                // will list all datasets by owner. We check that the
                // principal's authorized set covers them. Collected into a set:
                // with tenant/role inheritance the authorized list can be
                // tenant-wide, and the cover check below is per dataset.
                //
                // The set is tenant-filtered like Python's `delete_all`, which
                // enumerates `get_authorized_existing_datasets([], "delete",
                // user)` (`cognee/api/v1/datasets/datasets.py:316-323@37f788bd2`
                // → `get_all_user_permission_datasets.py:49-53`). An owner
                // dataset in another tenant is therefore not covered and the
                // whole request fails closed.
                let authorized: HashSet<Uuid> = self
                    .acl_db
                    .authorized_dataset_ids_in_tenant(principal_id, "delete", tenant_id)
                    .await
                    .map_err(|e| DeleteError::Runtime(format!("ACL query failed: {e}")))?
                    .into_iter()
                    .collect();

                let owner_datasets = self
                    .database
                    .list_datasets_by_owner(*owner_id)
                    .await
                    .map_err(|e| {
                        DeleteError::Runtime(format!("Failed to list owner datasets: {e}"))
                    })?;

                // The denial names no dataset: the uncovered one may belong to
                // another tenant (or another owner) the caller cannot see.
                if let Some(ds) = owner_datasets
                    .iter()
                    .find(|ds| !authorized.contains(&ds.id))
                {
                    tracing::warn!(
                        principal = %principal_id,
                        dataset = %ds.id,
                        "user-scope delete denied: dataset not covered by the caller's delete grants in its tenant"
                    );
                    return Err(DeleteError::PermissionDenied(
                        "delete permission denied for the requested scope".to_string(),
                    ));
                }
            }
            // Enumerated in-tenant by `execute` / `preview` and never reaches
            // here; a no-op arm keeps the match exhaustive.
            DeleteScope::All => {}
        }

        Ok(())
    }
}

/// `request` with its scope replaced by `DatasetById { dataset_id }`.
fn by_id(request: &DeleteRequest, dataset_id: Uuid) -> DeleteRequest {
    DeleteRequest {
        scope: DeleteScope::DatasetById { dataset_id },
        mode: request.mode.clone(),
        memory_only: request.memory_only,
    }
}

impl AuthorizedDeleteService {
    /// Look up a dataset by name + owner and return its ID.
    async fn resolve_dataset_id(
        &self,
        owner_id: Uuid,
        dataset_name: &str,
    ) -> Result<Uuid, DeleteError> {
        let dataset = self
            .database
            .get_dataset_by_name(dataset_name, owner_id, None)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!("Failed to resolve dataset '{dataset_name}': {e}"))
            })?
            .ok_or_else(|| {
                DeleteError::Validation(format!(
                    "Dataset '{dataset_name}' was not found for owner {owner_id}"
                ))
            })?;

        Ok(dataset.id)
    }

    /// Check that the principal has "delete" permission on the dataset in
    /// `tenant_id` — Python `get_authorized_dataset(user, id, "delete")` →
    /// `get_specific_user_permission_datasets`
    /// (`cognee/modules/data/methods/get_authorized_dataset.py:11-29`,
    /// `cognee/modules/users/permissions/methods/get_specific_user_permission_datasets.py:27-38`
    /// @37f788bd2). No grant and a grant only in another tenant answer the
    /// same `PermissionDenied`.
    async fn require_delete_permission(
        &self,
        principal_id: Uuid,
        tenant_id: Option<Uuid>,
        dataset_id: Uuid,
    ) -> Result<(), DeleteError> {
        let has_perm = self
            .acl_db
            .has_permission_in_tenant(principal_id, dataset_id, "delete", tenant_id)
            .await
            .map_err(|e| DeleteError::Runtime(format!("ACL check failed: {e}")))?;

        if !has_perm {
            return Err(DeleteError::PermissionDenied(format!(
                "Principal {principal_id} does not have 'delete' permission on dataset {dataset_id}"
            )));
        }

        Ok(())
    }
}
