use {
    crate::error::{IndexerError, Result},
    aws_config::{BehaviorVersion, Region, retry::RetryConfig, timeout::TimeoutConfig},
    aws_credential_types::Credentials,
    aws_sdk_s3::{
        Client as AwsS3Client,
        config::Builder as S3ConfigBuilder,
        error::SdkError,
        operation::{
            create_bucket::CreateBucketError, get_object::GetObjectError,
            head_bucket::HeadBucketError, head_object::HeadObjectError,
        },
        primitives::ByteStream,
    },
    serde::{Deserialize, Serialize},
    std::{path::Path, time::Duration},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct S3Config {
    pub enabled: bool,
    pub path: String,
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
    pub region: String,
}

impl S3Config {
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    async fn client(&self) -> Result<AwsS3Client> {
        let creds = Credentials::new(
            self.access_key.clone(),
            self.secret_key.clone(),
            None,
            None,
            "static",
        );

        // Bound every S3 operation in wall-clock terms. Without this the SDK
        // inherits its built-in defaults (effectively no read timeout for
        // large bodies, ~30s connect), so a network black-hole — DNS that
        // returns timeouts instead of NXDOMAIN, an unreachable bucket, a
        // stalled TLS handshake — leaves each call hanging until something
        // upstream gives up. Combined with the per-block retry loop in
        // `sync_block_to_s3`, that turns a single failed bucket into minutes
        // of wasted work per cron firing and processes pile up.
        //
        // `operation_timeout` caps the whole call including SDK-level retries;
        // `operation_attempt_timeout` caps a single attempt. With
        // `RetryConfig::standard().with_max_attempts(3)` the worst case is
        // bounded at ~30s per S3 op.
        let timeout_cfg = TimeoutConfig::builder()
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(10))
            .operation_attempt_timeout(Duration::from_secs(15))
            .operation_timeout(Duration::from_secs(30))
            .build();

        let retry_cfg = RetryConfig::standard()
            .with_max_attempts(3)
            .with_initial_backoff(Duration::from_millis(200));

        let base_cfg = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(self.region.clone()))
            .credentials_provider(creds)
            .timeout_config(timeout_cfg)
            .retry_config(retry_cfg)
            .load()
            .await;

        let mut builder = S3ConfigBuilder::from(&base_cfg);

        if !self.endpoint.is_empty() {
            builder = builder.endpoint_url(self.endpoint.clone());
        }

        Ok(AwsS3Client::from_conf(builder.build()))
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    cfg: S3Config,
    inner: AwsS3Client,
}

impl Client {
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn new(cfg: S3Config) -> Result<Self> {
        let inner = cfg.client().await?;
        Ok(Self { cfg, inner })
    }

    /// Create the configured bucket. Idempotent: a bucket that already exists
    /// (whether owned by us or, on AWS, anyone) is treated as success, so this
    /// is safe to call on every startup. Any other failure is returned as an
    /// error.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(bucket = %self.cfg.bucket)))]
    pub async fn create_bucket(&self) -> Result<()> {
        match self
            .inner
            .create_bucket()
            .bucket(&self.cfg.bucket)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            // Already present — nothing to do.
            Err(SdkError::ServiceError(inner))
                if matches!(
                    inner.err(),
                    CreateBucketError::BucketAlreadyOwnedByYou(_)
                        | CreateBucketError::BucketAlreadyExists(_)
                ) =>
            {
                Ok(())
            },
            Err(e) => Err(IndexerError::s3(e.to_string())),
        }
    }

    /// Whether the configured bucket exists and is reachable. A `NotFound`
    /// response means it's absent; any other failure (transport, auth) is
    /// returned as an error rather than reported as "missing".
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(bucket = %self.cfg.bucket)))]
    pub async fn bucket_exists(&self) -> Result<bool> {
        let res = self
            .inner
            .head_bucket()
            .bucket(&self.cfg.bucket)
            .send()
            .await;

        Ok(match res {
            Ok(_) => true,
            // Service error and it's specifically a NotFound => doesn't exist.
            Err(SdkError::ServiceError(inner))
                if matches!(inner.err(), HeadBucketError::NotFound(_)) =>
            {
                false
            },
            // anything else is a real error
            Err(e) => return Err(IndexerError::s3(e.to_string())),
        })
    }

    /// Upload an in-memory buffer directly, without staging it to a file. Useful
    /// for objects built in memory (e.g. a freshly compressed batch archive).
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(key = %key)))]
    pub async fn upload_bytes(&self, key: String, data: Vec<u8>) -> Result<()> {
        self.put(key, ByteStream::from(data)).await
    }

    /// Shared `put_object` call backing [`Self::upload_bytes`].
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(key = %key)))]
    async fn put(&self, key: String, body: ByteStream) -> Result<()> {
        self.inner
            .put_object()
            .bucket(&self.cfg.bucket)
            .key(key)
            .body(body)
            .send()
            .await
            .map_err(|e| IndexerError::s3(e.to_string()))?;

        Ok(())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(key = %key)))]
    pub async fn exists(&self, key: &str) -> Result<bool> {
        let res = self
            .inner
            .head_object()
            .bucket(&self.cfg.bucket)
            .key(key)
            .send()
            .await;

        Ok(match res {
            Ok(_) => true,
            // Service error and it's specifically a NotFound => doesn't exist
            Err(SdkError::ServiceError(inner))
                if matches!(inner.err(), HeadObjectError::NotFound(_)) =>
            {
                false
            },
            // anything else is a real error
            Err(e) => return Err(IndexerError::s3(e.to_string())),
        })
    }

    /// List object keys (and their sizes in bytes) under a prefix, following
    /// pagination until the bucket is exhausted.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(prefix = %prefix)))]
    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<(String, i64)>> {
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut req = self
                .inner
                .list_objects_v2()
                .bucket(&self.cfg.bucket)
                .prefix(prefix);

            if let Some(token) = continuation.clone() {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| IndexerError::s3(e.to_string()))?;

            for obj in resp.contents() {
                if let Some(key) = obj.key() {
                    keys.push((key.to_string(), obj.size().unwrap_or_default()));
                }
            }

            if resp.is_truncated().unwrap_or(false) {
                continuation = resp.next_continuation_token().map(ToString::to_string);
            } else {
                break;
            }
        }

        Ok(keys)
    }

    /// Fetch an object fully into memory. Returns `None` if the object does not
    /// exist (`NoSuchKey`) — distinct from a transport/auth error, which is
    /// returned as `Err`.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(key = %key)))]
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let resp = match self
            .inner
            .get_object()
            .bucket(&self.cfg.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(resp) => resp,
            // Object genuinely absent — distinct from a transport/auth failure.
            Err(SdkError::ServiceError(inner))
                if matches!(inner.err(), GetObjectError::NoSuchKey(_)) =>
            {
                return Ok(None);
            },
            Err(e) => return Err(IndexerError::s3(e.to_string())),
        };

        let data = resp
            .body
            .collect()
            .await
            .map_err(|e| IndexerError::byte_stream(e.to_string()))?
            .into_bytes();

        Ok(Some(data.to_vec()))
    }

    /// Download an object to `dest`, creating parent directories as needed.
    /// Returns the number of bytes written, or `None` if the object does not
    /// exist (`NoSuchKey`) — callers can treat that as a genuine gap rather than
    /// a retryable error.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, fields(key = %key)))]
    pub async fn download_file(&self, key: &str, dest: &Path) -> Result<Option<u64>> {
        let Some(data) = self.get(key).await? else {
            return Ok(None);
        };

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| IndexerError::s3(e.to_string()))?;
        }

        std::fs::write(dest, &data).map_err(|e| IndexerError::s3(e.to_string()))?;

        Ok(Some(data.len() as u64))
    }
}
