use aws_credential_types::Credentials;
use aws_sdk_s3::{config::Region, primitives::ByteStream, Client};

/// Thin wrapper around aws-sdk-s3 configured for Cloudflare R2's S3-compatible
/// endpoint. We use path-style addressing and `auto` region (R2 ignores region
/// but the SDK requires one).
///
/// Clone is cheap — aws-sdk-s3::Client is Arc-backed under the hood.
#[derive(Clone)]
pub struct R2Client {
    client: Client,
    bucket: String,
}

impl R2Client {
    pub async fn from_env() -> anyhow::Result<Self> {
        let endpoint = std::env::var("R2_ENDPOINT")
            .map_err(|_| anyhow::anyhow!("R2_ENDPOINT not set"))?;
        let bucket = std::env::var("R2_BUCKET")
            .map_err(|_| anyhow::anyhow!("R2_BUCKET not set"))?;
        let access_key = std::env::var("R2_ACCESS_KEY_ID")
            .map_err(|_| anyhow::anyhow!("R2_ACCESS_KEY_ID not set"))?;
        let secret_key = std::env::var("R2_SECRET_ACCESS_KEY")
            .map_err(|_| anyhow::anyhow!("R2_SECRET_ACCESS_KEY not set"))?;

        let creds = Credentials::new(access_key, secret_key, None, None, "clsi-rs-env");
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(Region::new("auto"))
            .endpoint_url(endpoint)
            .credentials_provider(creds)
            .load()
            .await;
        let s3_cfg = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(true)
            .build();
        let client = Client::from_conf(s3_cfg);

        Ok(Self { client, bucket })
    }

    pub async fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> anyhow::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("r2 put {key}: {e}"))?;
        Ok(())
    }

    /// Delete every object under a prefix. Used on clear-project.
    /// Batched 1000 at a time (R2's limit per DeleteObjects).
    pub async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<()> {
        let mut continuation: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = &continuation {
                req = req.continuation_token(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("r2 list {prefix}: {e}"))?;

            let contents = resp.contents();
            if !contents.is_empty() {
                let ids: Vec<_> = contents
                    .iter()
                    .filter_map(|o| o.key())
                    .map(|k| {
                        aws_sdk_s3::types::ObjectIdentifier::builder()
                            .key(k)
                            .build()
                            .expect("key set")
                    })
                    .collect();
                let delete = aws_sdk_s3::types::Delete::builder()
                    .set_objects(Some(ids))
                    .build()
                    .expect("objects set");
                self.client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("r2 delete {prefix}: {e}"))?;
            }

            if resp.is_truncated().unwrap_or(false) {
                continuation = resp.next_continuation_token().map(|s| s.to_owned());
            } else {
                break;
            }
        }
        Ok(())
    }
}
