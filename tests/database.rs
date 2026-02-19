use mongodb::bson::DateTime;
use risc_v_sim_web::database::{DatabaseService, SubmissionRecord, SubmissionStatus};

#[tokio::test]
async fn database_create_and_retrieve_submission() {
    let db_service = DatabaseService::new().await.unwrap();

    let test_uuid = format!("test-{}", ulid::Ulid::new());
    let test_user_id: i64 = 123456;

    let submission = SubmissionRecord {
        id: None,
        uuid: test_uuid.clone(),
        user_id: test_user_id,
        status: SubmissionStatus::Awaits,
        created_at: DateTime::now(),
        updated_at: DateTime::now(),
    };

    let created_id = db_service
        .create_submission(submission.clone())
        .await
        .unwrap();
    assert!(!created_id.to_hex().is_empty());

    let retrieved = db_service.get_submission_by_uuid(&test_uuid).await.unwrap();
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.uuid, test_uuid);
    assert_eq!(retrieved.user_id, test_user_id);
    assert_eq!(retrieved.status, SubmissionStatus::Awaits);

    db_service
        .update_submission_status(&test_uuid, SubmissionStatus::InProgress)
        .await
        .unwrap();

    let updated = db_service
        .get_submission_by_uuid(&test_uuid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, SubmissionStatus::InProgress);

    let user_submissions = db_service.get_user_submissions(test_user_id).await.unwrap();
    assert!(!user_submissions.is_empty());
    assert!(user_submissions.iter().any(|s| s.uuid == test_uuid));

    let cleanup_result = db_service
        .submissions_collection()
        .delete_one(mongodb::bson::doc! {"uuid": &test_uuid})
        .await
        .unwrap();
    assert_eq!(cleanup_result.deleted_count, 1);
}
