use std::time::Duration;

use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;

/// Thin wrapper around the S3 client pointed at MinIO (architecture.md:
/// object storage for event file attachments). MinIO speaks the S3 API, so
/// the AWS SDK works unmodified against it via `endpoint_url` +
/// `force_path_style` (MinIO doesn't support virtual-hosted-style bucket
/// URLs by default).
#[derive(Clone)]
pub struct Storage {
    client: Client,
    bucket: String,
}

/// Short-lived presigned URLs only (architecture.md's "Uploads (MinIO)"
/// section) — no public bucket, no long-lived links.
const PRESIGNED_URL_TTL: Duration = Duration::from_secs(300);

impl Storage {
    pub fn new(client: Client, bucket: String) -> Self {
        Self { client, bucket }
    }

    pub async fn from_env() -> anyhow::Result<Self> {
        let endpoint = std::env::var("MINIO_ENDPOINT")?;
        let access_key = std::env::var("MINIO_ACCESS_KEY")?;
        let secret_key = std::env::var("MINIO_SECRET_KEY")?;
        let bucket = std::env::var("MINIO_BUCKET")?;
        let region = std::env::var("MINIO_REGION").unwrap_or_else(|_| "us-east-1".into());

        let credentials = Credentials::new(access_key, secret_key, None, None, "minio-static");
        let config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region))
            .endpoint_url(endpoint)
            .credentials_provider(credentials)
            .force_path_style(true)
            .build();

        Ok(Self::new(Client::from_conf(config), bucket))
    }

    /// Uploads bytes at `key`, validated by the caller beforehand (MIME
    /// sniffed from content, not extension; size checked before this call).
    pub async fn put_object(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> anyhow::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(body))
            .content_type(content_type)
            .send()
            .await?;
        Ok(())
    }

    /// Short-lived presigned GET URL — never a public bucket link.
    pub async fn presigned_get_url(&self, key: &str) -> anyhow::Result<String> {
        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(PresigningConfig::expires_in(PRESIGNED_URL_TTL)?)
            .await?;
        Ok(presigned.uri().to_string())
    }

    /// Deletes many keys in as few round trips as S3 allows. Callers run
    /// this inside an open transaction (see `agenda::attachments::
    /// delete_objects`), so a key-at-a-time loop would hold that
    /// transaction and its locks open for one network round trip per
    /// attachment — a group delete has no bound on how many that is.
    ///
    /// An empty list makes no request at all: S3 rejects a `Delete` with
    /// no objects in it, and "nothing to delete" is the common case.
    pub async fn delete_objects(&self, keys: &[String]) -> anyhow::Result<()> {
        for chunk in keys.chunks(DELETE_OBJECTS_MAX_KEYS) {
            let mut delete = Delete::builder();
            for key in chunk {
                delete = delete.objects(ObjectIdentifier::builder().key(key).build()?);
            }

            let response = self
                .client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete.build()?)
                .send()
                .await?;

            // A 200 is not proof the keys are gone: `DeleteObjects`
            // reports per-key failures in the response body. Letting one
            // through would defeat the whole point of deleting the objects
            // before the rows.
            if let Some(report) = delete_errors_report(response.errors()) {
                anyhow::bail!("delete_objects: {report}");
            }
        }
        Ok(())
    }
}

/// S3 caps a single `DeleteObjects` request at 1000 keys.
const DELETE_OBJECTS_MAX_KEYS: usize = 1000;

/// Turns the per-key failures `DeleteObjects` returns in its response body
/// into a message, or `None` when every key was deleted. Callers treat
/// `Some` as a failed delete.
fn delete_errors_report(errors: &[aws_sdk_s3::types::Error]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let detail = errors
        .iter()
        .map(|e| {
            format!(
                "{} ({}: {})",
                e.key().unwrap_or("<no key>"),
                e.code().unwrap_or("<no code>"),
                e.message().unwrap_or("<no message>"),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("{} object(s) not deleted: {detail}", errors.len()))
}

/// Restricted set of file types accepted for event attachments
/// (architecture.md's "Uploads (MinIO)" section). Sniffed from the actual
/// bytes via `infer`, never trusted from the client-supplied filename/MIME.
const ALLOWED_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "application/pdf"];

pub const MAX_ATTACHMENT_SIZE_BYTES: usize = 20 * 1024 * 1024;

/// Sniffs the real MIME type from file content and rejects anything outside
/// the allow-list, regardless of what the client claims.
pub fn sniff_and_validate_mime(bytes: &[u8]) -> Option<&'static str> {
    let kind = infer::get(bytes)?;
    let mime = kind.mime_type();
    ALLOWED_MIME_TYPES.iter().find(|&&m| m == mime).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::types::Error as S3Error;

    #[test]
    fn a_response_with_no_errors_reports_nothing() {
        assert!(delete_errors_report(&[]).is_none());
    }

    #[test]
    fn the_report_names_every_key_that_was_not_deleted() {
        let errors = [
            S3Error::builder()
                .key("group/event/one")
                .code("AccessDenied")
                .message("Access Denied")
                .build(),
            S3Error::builder()
                .key("group/event/two")
                .code("InternalError")
                .message("we encountered an internal error")
                .build(),
        ];

        let report = delete_errors_report(&errors).expect("a non-empty error list must report");

        assert!(report.contains("group/event/one"), "{report}");
        assert!(report.contains("group/event/two"), "{report}");
        assert!(report.contains("AccessDenied"), "{report}");
        assert!(report.contains('2'), "{report}");
    }

    /// The three fields are all optional on the wire. A key that fails
    /// without naming itself is still a failure — reporting it as one
    /// matters more than the missing detail, since the alternative is
    /// treating the delete as a success and leaking the object.
    #[test]
    fn an_error_missing_its_fields_still_reports() {
        let report = delete_errors_report(&[S3Error::builder().build()])
            .expect("an error with no fields is still an error");
        assert!(report.contains('1'), "{report}");
    }

    /// An empty key list must not reach S3 at all: `DeleteObjects` rejects
    /// an empty `Delete` element (`MalformedXML`), and "delete a group /
    /// event that has no attachments" is the common case. The storage here
    /// points at an unreachable endpoint, so any request at all would
    /// surface as an error.
    #[tokio::test]
    async fn deleting_an_empty_key_list_never_calls_storage() {
        let credentials = Credentials::new("test", "test", None, None, "minio-static");
        let config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .endpoint_url("http://127.0.0.1:1")
            .credentials_provider(credentials)
            .force_path_style(true)
            .build();
        let storage = Storage::new(Client::from_conf(config), "test-bucket".into());

        storage
            .delete_objects(&[])
            .await
            .expect("no keys means no request, not a failed one");
    }
}
