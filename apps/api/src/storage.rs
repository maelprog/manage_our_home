use std::time::Duration;

use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
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
    pub async fn put_object(&self, key: &str, body: Vec<u8>, content_type: &str) -> anyhow::Result<()> {
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

    pub async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await?;
        Ok(())
    }
}

/// Restricted set of file types accepted for event attachments
/// (architecture.md's "Uploads (MinIO)" section). Sniffed from the actual
/// bytes via `infer`, never trusted from the client-supplied filename/MIME.
const ALLOWED_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "application/pdf",
];

pub const MAX_ATTACHMENT_SIZE_BYTES: usize = 20 * 1024 * 1024;

/// Sniffs the real MIME type from file content and rejects anything outside
/// the allow-list, regardless of what the client claims.
pub fn sniff_and_validate_mime(bytes: &[u8]) -> Option<&'static str> {
    let kind = infer::get(bytes)?;
    let mime = kind.mime_type();
    ALLOWED_MIME_TYPES.iter().find(|&&m| m == mime).copied()
}
