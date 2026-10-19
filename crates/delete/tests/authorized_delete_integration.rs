#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `AuthorizedDeleteService` ACL enforcement.
//!
//! These tests verify that the ACL wrapper around `DeleteService` correctly
//! denies or allows operations based on the principal's permissions.

use std::sync::Arc;

use cognee_database::{self as database, AclDb, DatabaseConnection, DeleteDb};
use cognee_delete::{
    AuthorizedDeleteService, DeleteError, DeleteMode, DeleteRequest, DeleteScope, DeleteService,
};
use cognee_models::{Data, Dataset};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::MockAclDb;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create an in-memory SQLite database, run migrations, and return it
/// alongside a `MockStorage` and a fresh `MockAclDb` (the in-memory test
/// ACL backend).
async fn setup() -> (Arc<DatabaseConnection>, Arc<MockStorage>, Arc<MockAclDb>) {
    let db = database::connect("sqlite::memory:").await.unwrap();
    database::initialize(&db).await.unwrap();
    let storage = Arc::new(MockStorage::new());
    let acl = Arc::new(MockAclDb::new());
    (Arc::new(db), storage, acl)
}

/// Seed one dataset + one data item into the database, attach them together,
/// and write a small file into mock storage. Returns `(dataset_id, data_id)`.
async fn seed_dataset_with_data(
    db: &DatabaseConnection,
    storage: &MockStorage,
    owner_id: Uuid,
    dataset_name: &str,
) -> (Uuid, Uuid) {
    seed_dataset_with_data_in(db, storage, owner_id, dataset_name, None).await
}

/// [`seed_dataset_with_data`] with the dataset row carrying `tenant_id`.
async fn seed_dataset_with_data_in(
    db: &DatabaseConnection,
    storage: &MockStorage,
    owner_id: Uuid,
    dataset_name: &str,
    tenant_id: Option<Uuid>,
) -> (Uuid, Uuid) {
    let dataset = Dataset::new(
        dataset_name.to_string(),
        owner_id,
        tenant_id,
        Uuid::new_v4(),
    );
    let dataset_id = dataset.id;
    database::ops::datasets::create_dataset(db, dataset)
        .await
        .unwrap();

    let location = storage.store(b"test content", "test.txt").await.unwrap();

    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "test.txt",
        &location,
        "file://test.txt",
        "txt",
        "text/plain",
        "hash_placeholder",
        owner_id,
    )
    .build();
    database::ops::data::create_data(db, data).await.unwrap();
    database::ops::datasets::attach_data_to_dataset(db, dataset_id, data_id)
        .await
        .unwrap();

    (dataset_id, data_id)
}

/// Grant `"delete"` permission on `dataset_id` to `principal_id` against the
/// supplied ACL store. The store is shared with the `AuthorizedDeleteService`
/// so grants persisted here are visible to the service's permission checks.
async fn grant_delete_permission(acl: &Arc<MockAclDb>, principal_id: Uuid, dataset_id: Uuid) {
    let acl_dyn: &dyn AclDb = acl.as_ref();
    acl_dyn
        .ensure_principal(principal_id, "user")
        .await
        .unwrap();
    acl_dyn
        .grant_permission(principal_id, dataset_id, "delete")
        .await
        .unwrap();
}

/// Build an `AuthorizedDeleteService` from the shared database, storage, and
/// ACL store. The same `acl` `Arc` must be reused by `grant_delete_permission`
/// so that grants made before service construction are visible.
fn build_authorized_service(
    db: &Arc<DatabaseConnection>,
    storage: &Arc<MockStorage>,
    acl: &Arc<MockAclDb>,
) -> AuthorizedDeleteService {
    let inner = DeleteService::new(
        storage.clone() as Arc<dyn cognee_storage::StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    AuthorizedDeleteService::new(
        inner,
        acl.clone() as Arc<dyn AclDb>,
        db.clone() as Arc<dyn DeleteDb>,
    )
}

// ---------------------------------------------------------------------------
// Test 1: ACL denied returns PermissionDenied
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acl_denied_returns_permission_denied() {
    let (db, storage, acl) = setup().await;
    let owner_id = Uuid::new_v4();

    let (dataset_id, data_id) =
        seed_dataset_with_data(&db, &storage, owner_id, "acl_denied_ds").await;

    let svc = build_authorized_service(&db, &storage, &acl);

    // The principal (owner_id) has NOT been granted "delete" permission.
    let result = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id,
                    data_id,
                    dataset_name: Some("acl_denied_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            },
            owner_id,
            None,
        )
        .await;

    assert!(result.is_err(), "execute should fail without ACL grant");
    let err = result.unwrap_err();
    assert!(
        matches!(err, DeleteError::PermissionDenied(_)),
        "expected PermissionDenied, got: {err:?}"
    );

    // Verify data still exists (nothing was deleted).
    let data = database::ops::data::get_data(&db, data_id).await.unwrap();
    assert!(
        data.is_some(),
        "data should still exist after denied delete"
    );

    let ds = database::ops::datasets::get_dataset(&db, dataset_id)
        .await
        .unwrap();
    assert!(
        ds.is_some(),
        "dataset should still exist after denied delete"
    );
}

// ---------------------------------------------------------------------------
// Test 2: ACL granted allows deletion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acl_granted_allows_deletion() {
    let (db, storage, acl) = setup().await;
    let owner_id = Uuid::new_v4();

    let (dataset_id, data_id) =
        seed_dataset_with_data(&db, &storage, owner_id, "acl_granted_ds").await;

    // Grant delete permission.
    grant_delete_permission(&acl, owner_id, dataset_id).await;

    let svc = build_authorized_service(&db, &storage, &acl);

    let result = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id,
                    data_id,
                    dataset_name: Some("acl_granted_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            },
            owner_id,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "execute should succeed with ACL grant: {result:?}"
    );
    let result = result.unwrap();
    assert!(
        result.deleted_data >= 1,
        "should have deleted at least 1 data record, got: {}",
        result.deleted_data
    );

    // Verify data is gone.
    let data = database::ops::data::get_data(&db, data_id).await.unwrap();
    assert!(
        data.is_none(),
        "data should be deleted after authorized delete"
    );
}

// ---------------------------------------------------------------------------
// Test 3: preview() respects ACL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preview_respects_acl() {
    let (db, storage, acl) = setup().await;
    let owner_id = Uuid::new_v4();

    let (_dataset_id, data_id) =
        seed_dataset_with_data(&db, &storage, owner_id, "preview_acl_ds").await;

    let svc = build_authorized_service(&db, &storage, &acl);

    // No permission granted — preview should be denied.
    let result = svc
        .preview(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id,
                    data_id,
                    dataset_name: Some("preview_acl_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            },
            owner_id,
            None,
        )
        .await;

    assert!(result.is_err(), "preview should fail without ACL grant");
    let err = result.unwrap_err();
    assert!(
        matches!(err, DeleteError::PermissionDenied(_)),
        "expected PermissionDenied, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: cross-user isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cross_user_isolation() {
    let (db, storage, acl) = setup().await;
    let owner_a = Uuid::new_v4();
    let owner_b = Uuid::new_v4();

    // Seed data for both owners.
    let (dataset_a_id, data_a_id) =
        seed_dataset_with_data(&db, &storage, owner_a, "user_a_ds").await;
    let (dataset_b_id, data_b_id) =
        seed_dataset_with_data(&db, &storage, owner_b, "user_b_ds").await;

    // Grant delete permission to owner_a only.
    grant_delete_permission(&acl, owner_a, dataset_a_id).await;
    // owner_b gets NO permission.

    let svc = build_authorized_service(&db, &storage, &acl);

    // owner_a deletes their own data — should succeed.
    let result_a = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner_a,
                    data_id: data_a_id,
                    dataset_name: Some("user_a_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            },
            owner_a,
            None,
        )
        .await;

    assert!(
        result_a.is_ok(),
        "owner_a should be able to delete their data: {result_a:?}"
    );
    assert!(
        result_a.unwrap().deleted_data >= 1,
        "owner_a should have deleted at least 1 data record"
    );

    // owner_b tries to delete their own data — should fail (no permission).
    let result_b = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner_b,
                    data_id: data_b_id,
                    dataset_name: Some("user_b_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            },
            owner_b,
            None,
        )
        .await;

    assert!(
        result_b.is_err(),
        "owner_b should be denied without ACL grant"
    );
    let err = result_b.unwrap_err();
    assert!(
        matches!(err, DeleteError::PermissionDenied(_)),
        "expected PermissionDenied for owner_b, got: {err:?}"
    );

    // Verify owner_b's data still exists.
    let data_b = database::ops::data::get_data(&db, data_b_id).await.unwrap();
    assert!(
        data_b.is_some(),
        "owner_b's data should still exist after denied delete"
    );

    let ds_b = database::ops::datasets::get_dataset(&db, dataset_b_id)
        .await
        .unwrap();
    assert!(
        ds_b.is_some(),
        "owner_b's dataset should still exist after denied delete"
    );

    // Also verify owner_a's data is actually gone.
    let data_a = database::ops::data::get_data(&db, data_a_id).await.unwrap();
    assert!(
        data_a.is_none(),
        "owner_a's data should be gone after successful delete"
    );
}

// ---------------------------------------------------------------------------
// Tenant scoping (W1/W2): a `delete` grant counts only on a dataset in the
// caller's tenant, strict equality, `None` ⇔ NULL — Python
// `get_all_user_permission_datasets.py:49-53@37f788bd2`, reached from
// `get_authorized_dataset` (by id) and `delete_all` (`datasets.py:316-323`).
//
// Each fixture seeds the DB row with the tenant (the inner `DeleteService`
// reads DB rows) **and** registers it with the mock (`MockAclDb` filters on
// its own map) — otherwise the tests would pass vacuously.
// ---------------------------------------------------------------------------

/// Seed a dataset in `tenant` (DB row + mock registration). Returns
/// `(dataset_id, data_id)`.
async fn seed_in_tenant(
    db: &DatabaseConnection,
    storage: &MockStorage,
    acl: &MockAclDb,
    owner_id: Uuid,
    name: &str,
    tenant: Option<Uuid>,
) -> (Uuid, Uuid) {
    let ids = seed_dataset_with_data_in(db, storage, owner_id, name, tenant).await;
    if let Some(t) = tenant {
        acl.set_dataset_tenant(ids.0, t);
    }
    ids
}

fn request(scope: DeleteScope) -> DeleteRequest {
    DeleteRequest {
        scope,
        mode: DeleteMode::Soft,
        memory_only: false,
    }
}

fn assert_denied<T: std::fmt::Debug>(result: Result<T, DeleteError>, what: &str) {
    match result {
        Err(DeleteError::PermissionDenied(_)) => {}
        other => panic!("{what}: expected PermissionDenied, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_grant_in_another_tenant_is_denied_like_no_grant_by_id() {
    let (db, storage, acl) = setup().await;
    let caller = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (granted_ds, granted_data) =
        seed_in_tenant(&db, &storage, &acl, caller, "t2_granted", Some(t2)).await;
    let (ungranted_ds, ungranted_data) =
        seed_in_tenant(&db, &storage, &acl, caller, "t2_ungranted", Some(t2)).await;
    grant_delete_permission(&acl, caller, granted_ds).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    for (ds, data, what) in [
        (granted_ds, granted_data, "grant only in T2"),
        (ungranted_ds, ungranted_data, "no grant"),
    ] {
        assert_denied(
            svc.execute(
                &request(DeleteScope::DatasetById { dataset_id: ds }),
                caller,
                Some(t1),
            )
            .await,
            &format!("DatasetById, {what}"),
        );
        assert_denied(
            svc.execute(
                &request(DeleteScope::DataInDataset {
                    dataset_id: ds,
                    data_id: data,
                    delete_dataset_if_empty: false,
                }),
                caller,
                Some(t1),
            )
            .await,
            &format!("DataInDataset, {what}"),
        );
        assert!(
            database::ops::data::get_data(&db, data)
                .await
                .unwrap()
                .is_some(),
            "{what}: data must survive the denied delete"
        );
        assert!(
            database::ops::datasets::get_dataset(&db, ds)
                .await
                .unwrap()
                .is_some(),
            "{what}: dataset must survive the denied delete"
        );
    }
}

#[tokio::test]
async fn delete_grant_in_the_callers_tenant_is_admitted() {
    let (db, storage, acl) = setup().await;
    let caller = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (ds_a, data_a) = seed_in_tenant(&db, &storage, &acl, caller, "t2_a", Some(t2)).await;
    let (ds_b, _) = seed_in_tenant(&db, &storage, &acl, caller, "t2_b", Some(t2)).await;
    grant_delete_permission(&acl, caller, ds_a).await;
    grant_delete_permission(&acl, caller, ds_b).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    svc.execute(
        &request(DeleteScope::DataInDataset {
            dataset_id: ds_a,
            data_id: data_a,
            delete_dataset_if_empty: false,
        }),
        caller,
        Some(t2),
    )
    .await
    .expect("DataInDataset in the caller's tenant is admitted");
    svc.execute(
        &request(DeleteScope::DatasetById { dataset_id: ds_b }),
        caller,
        Some(t2),
    )
    .await
    .expect("DatasetById in the caller's tenant is admitted");
    assert!(
        database::ops::datasets::get_dataset(&db, ds_b)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn tenantless_caller_cannot_delete_a_tenanted_dataset_it_holds_delete_on() {
    let (db, storage, acl) = setup().await;
    let caller = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (tenanted, _) = seed_in_tenant(&db, &storage, &acl, caller, "tenanted", Some(t2)).await;
    let (untenanted, _) = seed_in_tenant(&db, &storage, &acl, caller, "untenanted", None).await;
    grant_delete_permission(&acl, caller, tenanted).await;
    grant_delete_permission(&acl, caller, untenanted).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    assert_denied(
        svc.execute(
            &request(DeleteScope::DatasetById {
                dataset_id: tenanted,
            }),
            caller,
            None,
        )
        .await,
        "tenant-less caller on a T2 dataset",
    );
    svc.execute(
        &request(DeleteScope::DatasetById {
            dataset_id: untenanted,
        }),
        caller,
        None,
    )
    .await
    .expect("tenant-less caller on a tenant-less dataset is admitted");
}

#[tokio::test]
async fn user_scope_cover_excludes_grants_in_another_tenant() {
    let (db, storage, acl) = setup().await;
    let owner = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (in_t1, data_t1) = seed_in_tenant(&db, &storage, &acl, owner, "in_t1", Some(t1)).await;
    let (in_t2, _) = seed_in_tenant(&db, &storage, &acl, owner, "in_t2", Some(t2)).await;
    grant_delete_permission(&acl, owner, in_t1).await;
    grant_delete_permission(&acl, owner, in_t2).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    assert_denied(
        svc.execute(
            &request(DeleteScope::User { owner_id: owner }),
            owner,
            Some(t1),
        )
        .await,
        "User scope with an owner dataset in T2",
    );
    assert!(
        database::ops::data::get_data(&db, data_t1)
            .await
            .unwrap()
            .is_some(),
        "nothing is deleted when the cover fails"
    );

    // Same fixture with every owner dataset in T1: the cover holds.
    let (db, storage, acl) = setup().await;
    let (a, _) = seed_in_tenant(&db, &storage, &acl, owner, "a", Some(t1)).await;
    let (b, _) = seed_in_tenant(&db, &storage, &acl, owner, "b", Some(t1)).await;
    grant_delete_permission(&acl, owner, a).await;
    grant_delete_permission(&acl, owner, b).await;
    let svc = build_authorized_service(&db, &storage, &acl);
    svc.execute(
        &request(DeleteScope::User { owner_id: owner }),
        owner,
        Some(t1),
    )
    .await
    .expect("User scope with every owner dataset in the caller's tenant is admitted");
}

/// `DeleteScope::All` is Python's `datasets.delete_all`
/// (`datasets.py:316-323@37f788bd2`): it deletes the caller's
/// `delete`-authorized datasets **in its tenant** and nothing else, so a
/// multi-tenant DB does not make it fail, and other tenants' datasets and
/// ungranted ones survive.
#[tokio::test]
async fn all_scope_deletes_only_the_callers_delete_authorized_datasets_in_its_tenant() {
    let (db, storage, acl) = setup().await;
    let caller = Uuid::new_v4();
    let other = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (in_t1, data_t1) = seed_in_tenant(&db, &storage, &acl, other, "in_t1", Some(t1)).await;
    let (in_t2, data_t2) = seed_in_tenant(&db, &storage, &acl, other, "in_t2", Some(t2)).await;
    let (ungranted_t1, _) = seed_in_tenant(&db, &storage, &acl, other, "ungranted", Some(t1)).await;
    grant_delete_permission(&acl, caller, in_t1).await;
    grant_delete_permission(&acl, caller, in_t2).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    let preview = svc
        .preview(&request(DeleteScope::All), caller, Some(t1))
        .await
        .expect("All preview in T1");
    assert_eq!(preview.datasets_to_delete, 1);

    let result = svc
        .execute(&request(DeleteScope::All), caller, Some(t1))
        .await
        .expect("All in T1 is not denied by T2's dataset");
    assert_eq!(result.deleted_datasets, 1);
    assert!(
        database::ops::datasets::get_dataset(&db, in_t1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        database::ops::data::get_data(&db, data_t1)
            .await
            .unwrap()
            .is_none()
    );
    for survivor in [in_t2, ungranted_t1] {
        assert!(
            database::ops::datasets::get_dataset(&db, survivor)
                .await
                .unwrap()
                .is_some(),
            "a dataset outside the caller's delete grants in T1 must survive"
        );
    }
    assert!(
        database::ops::data::get_data(&db, data_t2)
            .await
            .unwrap()
            .is_some()
    );

    // Tenant-less caller: holds grants only on tenanted datasets → nothing.
    let result = svc
        .execute(&request(DeleteScope::All), caller, None)
        .await
        .expect("All with nothing deletable is an empty success");
    assert_eq!(result.deleted_datasets, 0);
    assert!(
        database::ops::datasets::get_dataset(&db, in_t2)
            .await
            .unwrap()
            .is_some()
    );
}

/// A denied User cover must not name the uncovered dataset — it may be in a
/// tenant the caller cannot see.
#[tokio::test]
async fn user_scope_denial_does_not_name_the_uncovered_dataset() {
    let (db, storage, acl) = setup().await;
    let owner = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (in_t1, _) = seed_in_tenant(&db, &storage, &acl, owner, "visible_t1", Some(t1)).await;
    let _ = seed_in_tenant(&db, &storage, &acl, owner, "secret_t2_name", Some(t2)).await;
    grant_delete_permission(&acl, owner, in_t1).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    match svc
        .execute(
            &request(DeleteScope::User { owner_id: owner }),
            owner,
            Some(t1),
        )
        .await
    {
        Err(DeleteError::PermissionDenied(msg)) => {
            assert!(
                !msg.contains("secret_t2_name"),
                "leaks a dataset name: {msg}"
            );
        }
        other => panic!("expected PermissionDenied, got {other:?}"),
    }
}

#[tokio::test]
async fn preview_applies_the_tenant_too() {
    let (db, storage, acl) = setup().await;
    let caller = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let (ds, _) = seed_in_tenant(&db, &storage, &acl, caller, "t2_ds", Some(t2)).await;
    grant_delete_permission(&acl, caller, ds).await;
    let svc = build_authorized_service(&db, &storage, &acl);

    assert_denied(
        svc.preview(
            &request(DeleteScope::DatasetById { dataset_id: ds }),
            caller,
            Some(t1),
        )
        .await,
        "preview with a grant only in T2",
    );
    svc.preview(
        &request(DeleteScope::DatasetById { dataset_id: ds }),
        caller,
        Some(t2),
    )
    .await
    .expect("preview in the dataset's tenant is admitted");
}
