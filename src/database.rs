use anyhow::{Context, Result};
use futures_util::stream::TryStreamExt;
use mongodb::{
    Client, Collection, Database, IndexModel,
    bson::{Bson, DateTime, doc, oid::ObjectId},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionRecord {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<ObjectId>,
    pub uuid: String,
    pub user_id: i64,
    pub status: SubmissionStatus,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum SubmissionStatus {
    Completed,
    InProgress,
    Awaits,
}

impl From<SubmissionStatus> for Bson {
    fn from(status: SubmissionStatus) -> Self {
        match status {
            SubmissionStatus::Completed => Bson::String("Completed".to_string()),
            SubmissionStatus::InProgress => Bson::String("InProgress".to_string()),
            SubmissionStatus::Awaits => Bson::String("Awaits".to_string()),
        }
    }
}

#[derive(Clone)]
pub struct DatabaseService {
    db: Arc<Database>,
}

impl DatabaseService {
    pub async fn new() -> Result<Self> {
        let mongo_uri = std::env::var("MONGODB_URI")
            .unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
        let db_name = std::env::var("MONGODB_DB").unwrap_or_else(|_| "riscv_sim".to_string());

        let client = Client::with_uri_str(&mongo_uri)
            .await
            .context("Failed to connect to MongoDB")?;

        let db = Arc::new(client.database(&db_name));

        let submissions_collection: Collection<SubmissionRecord> = db.collection("submissions");
        submissions_collection
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "user_id": 1, "created_at": -1 })
                    .build(),
            )
            .await
            .context("Failed to create index on user_id and created_at")?;

        submissions_collection
            .create_index(IndexModel::builder().keys(doc! { "uuid": 1 }).build())
            .await
            .context("Failed to create index on uuid")?;

        Ok(DatabaseService { db })
    }

    pub fn submissions_collection(&self) -> Collection<SubmissionRecord> {
        self.db.collection("submissions")
    }

    pub async fn get_user_submissions(&self, user_id: i64) -> Result<Vec<SubmissionRecord>> {
        let collection = self.submissions_collection();
        let filter = doc! { "user_id": user_id };

        let mut cursor = collection
            .find(filter)
            .await
            .context("Failed to query user submissions")?;

        let mut submissions = Vec::new();
        while let Some(submission) = cursor.try_next().await? {
            submissions.push(submission);
        }

        Ok(submissions)
    }

    pub async fn create_submission(&self, submission: SubmissionRecord) -> Result<ObjectId> {
        let collection = self.submissions_collection();
        let result = collection
            .insert_one(submission)
            .await
            .context("Failed to create submission")?;

        Ok(result.inserted_id.as_object_id().unwrap())
    }

    pub async fn update_submission_status(
        &self,
        uuid: &str,
        status: SubmissionStatus,
    ) -> Result<()> {
        let collection = self.submissions_collection();
        let filter = doc! { "uuid": uuid };
        let update = doc! {
            "$set": {
                "status": Bson::from(status),
                "updated_at": DateTime::now(),
            }
        };

        collection
            .update_one(filter, update)
            .await
            .context("Failed to update submission status")?;

        Ok(())
    }

    pub async fn get_submission_by_uuid(&self, uuid: &str) -> Result<Option<SubmissionRecord>> {
        let collection = self.submissions_collection();
        let filter = doc! { "uuid": uuid };

        let submission = collection
            .find_one(filter)
            .await
            .context("Failed to get submission by uuid")?;

        Ok(submission)
    }

    pub async fn create_submission_with_user(
        &self,
        uuid: String,
        user_id: i64,
    ) -> Result<ObjectId> {
        let now = DateTime::now();
        let submission = SubmissionRecord {
            id: None,
            uuid,
            user_id,
            status: SubmissionStatus::Awaits,
            created_at: now,
            updated_at: now,
        };

        self.create_submission(submission).await
    }
}
