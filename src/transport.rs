use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::Client;
use std::io::{self, BufRead, Write};

use crate::message::Message;

pub struct Transport {
    s3: Option<Client>,
    bucket: Option<String>,
}

impl Transport {
    pub fn new() -> Self {
        Self {
            s3: None,
            bucket: None,
        }
    }

    fn configure(&mut self, fields: &std::collections::HashMap<String, String>) -> Result<()> {
        // Parse Config-Item fields from 601 message into a flat key=value map
        // e.g. "Acquire::r2::AccessKeyId=my-key" -> ("acquire::r2::accesskeyid", "my-key")
        let config: std::collections::HashMap<String, String> = fields
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("Config-Item"))
            .flat_map(|(_, v)| {
                v.lines().filter_map(|line| {
                    line.split_once('=')
                        .map(|(k, v)| (k.to_lowercase(), v.to_string()))
                })
            })
            .collect();

        let key_id = config
            .get("acquire::r2::accesskeyid")
            .context("Acquire::r2::AccessKeyId not set in apt.conf")?;
        let secret = config
            .get("acquire::r2::secretaccesskey")
            .context("Acquire::r2::SecretAccessKey not set in apt.conf")?;
        let endpoint = config
            .get("acquire::r2::endpointurl")
            .context("Acquire::r2::EndpointUrl not set in apt.conf")?;
        let bucket = config
            .get("acquire::r2::bucket")
            .context("Acquire::r2::Bucket not set in apt.conf")?;

        let credentials = Credentials::new(key_id, secret, None, None, "apt-conf");
        let s3_config = Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(Region::new("auto"))
            .endpoint_url(endpoint)
            .build();

        self.s3 = Some(Client::from_conf(s3_config));
        self.bucket = Some(bucket.clone());

        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Send capabilities
        out.write_all(
            Message::format(
                100,
                "Capabilities",
                &[
                    ("Version", "1.0"),
                    ("Single-Instance", "true"),
                    ("Send-Config", "true"), // tell APT to send 601 Configuration
                ],
            )
            .as_bytes(),
        )?;
        out.flush()?;

        let stdin = io::stdin();
        let mut buf = String::new();
        let mut reader = stdin.lock();

        loop {
            buf.clear();
            let mut block = String::new();

            loop {
                buf.clear();
                let n = reader.read_line(&mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                if buf.trim().is_empty() {
                    break;
                }
                block.push_str(&buf);
            }

            if block.is_empty() {
                continue;
            }

            if let Some(msg) = Message::parse(&block) {
                match msg.code {
                    601 => {
                        if let Err(e) = self.configure(&msg.fields) {
                            out.write_all(
                                Message::format(
                                    401,
                                    "General Failure",
                                    &[("Message", &e.to_string())],
                                )
                                .as_bytes(),
                            )?;
                            out.flush()?;
                            return Ok(());
                        }
                    }
                    600 => {
                        let uri = msg.fields.get("URI").cloned().unwrap_or_default();
                        let filename = msg.fields.get("Filename").cloned().unwrap_or_default();
                        self.acquire(&uri, &filename, &mut out).await?;
                    }
                    _ => {}
                }
            }
        }
    }

    async fn acquire(&self, uri: &str, filename: &str, out: &mut impl Write) -> Result<()> {
        let s3 = self
            .s3
            .as_ref()
            .context("S3 client not initialized — missing 601 Configuration?")?;
        let bucket = self.bucket.as_ref().context("Bucket not configured")?;

        let key = uri
            .trim_start_matches("r2://") // "apt-private/pool/main/pkg.deb"
            .split_once('/') // Some(("apt-private", "pool/main/pkg.deb"))
            .map(|(_, path)| path) // "pool/main/pkg.deb"
            .unwrap_or("")
            .to_string();

        out.write_all(Message::format(200, "URI Start", &[("URI", uri)]).as_bytes())?;
        out.flush()?;

        match s3.get_object().bucket(bucket).key(&key).send().await {
            Ok(resp) => {
                let bytes = resp.body.collect().await?.into_bytes();
                std::fs::write(filename, &bytes)?;

                let size = bytes.len().to_string();
                out.write_all(
                    Message::format(
                        201,
                        "URI Done",
                        &[("URI", uri), ("Filename", filename), ("Size", &size)],
                    )
                    .as_bytes(),
                )?;
            }
            Err(e) => {
                out.write_all(
                    Message::format(
                        400,
                        "URI Failure",
                        &[("URI", uri), ("Message", &format!("{:?}", e))],
                    )
                    .as_bytes(),
                )?;
            }
        }

        out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Transport;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::time::Duration;
    use tempfile::TempDir;

    const ACCESS_KEY: &str = "minioadmin";
    const SECRET_KEY: &str = "minioadmin";
    const BUCKET: &str = "apt-private";

    static MINIO_BIN: OnceLock<PathBuf> = OnceLock::new();
    static DOWNLOAD_LOCK: Mutex<()> = Mutex::new(());

    struct MinioInstance {
        process: Child,
        pub endpoint: String,
        _data_dir: TempDir, // kept alive to avoid cleanup during test
    }

    impl MinioInstance {
        fn ensure_binary() -> PathBuf {
            // Serialize concurrent download attempts
            let _guard = DOWNLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            MINIO_BIN
                .get_or_init(|| {
                    let bin_path =
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/minio");

                    if bin_path.exists() {
                        return bin_path;
                    }

                    println!("Downloading minio binary...");

                    let url = if cfg!(target_os = "macos") {
                        if cfg!(target_arch = "aarch64") {
                            "https://dl.min.io/server/minio/release/darwin-arm64/minio"
                        } else {
                            "https://dl.min.io/server/minio/release/darwin-amd64/minio"
                        }
                    } else if cfg!(target_arch = "aarch64") {
                        "https://dl.min.io/server/minio/release/linux-arm64/minio"
                    } else {
                        "https://dl.min.io/server/minio/release/linux-amd64/minio"
                    };

                    let status = Command::new("curl")
                        .args(["-fsSL", "-o", bin_path.to_str().unwrap(), url])
                        .status()
                        .expect("failed to run curl");

                    assert!(status.success(), "minio download failed");

                    Command::new("chmod")
                        .args(["+x", bin_path.to_str().unwrap()])
                        .status()
                        .expect("failed to chmod minio");

                    bin_path
                })
                .clone()
        }

        async fn start() -> Self {
            let minio_bin = Self::ensure_binary();
            let data_dir = TempDir::new().unwrap();
            let port = pick_free_port();
            let endpoint = format!("http://127.0.0.1:{}", port);

            let process = Command::new(&minio_bin)
                .args([
                    "server",
                    data_dir.path().to_str().unwrap(),
                    "--address",
                    &format!("127.0.0.1:{}", port),
                ])
                .env("MINIO_ROOT_USER", ACCESS_KEY)
                .env("MINIO_ROOT_PASSWORD", SECRET_KEY)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("failed to start minio");

            // Wait for minio to be ready
            wait_until_ready(&endpoint).await;

            let instance = Self {
                process,
                endpoint: endpoint.clone(),
                _data_dir: data_dir,
            };
            instance.create_bucket().await;
            instance
        }

        async fn create_bucket(&self) {
            make_s3_client(&self.endpoint)
                .await
                .create_bucket()
                .bucket(BUCKET)
                .send()
                .await
                .unwrap();
        }

        async fn upload(&self, key: &str, content: &[u8]) {
            make_s3_client(&self.endpoint)
                .await
                .put_object()
                .bucket(BUCKET)
                .key(key)
                .body(content.to_vec().into())
                .send()
                .await
                .unwrap();
        }
    }

    impl Drop for MinioInstance {
        fn drop(&mut self) {
            self.process.kill().ok();
        }
    }

    fn pick_free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

async fn wait_until_ready(endpoint: &str) {
    let url = format!("{}/minio/health/live", endpoint);
    for _ in 0..20 {
        if reqwest::get(&url).await.map(|r| r.status().is_success()).unwrap_or(false) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("minio did not become ready in time");
}

    async fn make_s3_client(endpoint: &str) -> aws_sdk_s3::Client {
        let config = aws_config::from_env()
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .credentials_provider(aws_credential_types::Credentials::new(
                ACCESS_KEY, SECRET_KEY, None, None, "test",
            ))
            .load()
            .await;
        aws_sdk_s3::Client::new(&config)
    }

    fn make_config(endpoint: &str) -> HashMap<String, String> {
        let items = [
            format!("Acquire::r2::AccessKeyId={}", ACCESS_KEY),
            format!("Acquire::r2::SecretAccessKey={}", SECRET_KEY),
            format!("Acquire::r2::EndpointUrl={}", endpoint),
            format!("Acquire::r2::Bucket={}", BUCKET),
        ]
        .join("\n");

        let mut fields = HashMap::new();
        fields.insert("Config-Item".to_string(), items);
        fields
    }

    #[tokio::test]
    async fn test_acquire_existing_file() {
        let minio = MinioInstance::start().await;
        minio.upload("pool/main/test_1.0_arm64.deb", b"fake deb content").await;

        let mut transport = Transport::new();
        transport.configure(&make_config(&minio.endpoint)).unwrap();

        let out_file = tempfile::NamedTempFile::new().unwrap();
        let mut output = Vec::new();

        transport
            .acquire(
                "r2://apt-private/pool/main/test_1.0_arm64.deb",
                out_file.path().to_str().unwrap(),
                &mut output,
            )
            .await
            .unwrap();

        let response = String::from_utf8(output).unwrap();
        assert!(response.contains("200 URI Start"));
        assert!(response.contains("201 URI Done"));
        assert!(response.contains("Size: 16"));
        assert_eq!(std::fs::read(out_file.path()).unwrap(), b"fake deb content");
    }

    #[tokio::test]
    async fn test_acquire_missing_file_returns_400() {
        let minio = MinioInstance::start().await;

        let mut transport = Transport::new();
        transport.configure(&make_config(&minio.endpoint)).unwrap();

        let out_file = tempfile::NamedTempFile::new().unwrap();
        let mut output = Vec::new();

        transport
            .acquire(
                "r2://apt-private/pool/main/nonexistent.deb",
                out_file.path().to_str().unwrap(),
                &mut output,
            )
            .await
            .unwrap();

        let response = String::from_utf8(output).unwrap();
        assert!(response.contains("200 URI Start"));
        assert!(response.contains("400 URI Failure"));
    }

    #[tokio::test]
    async fn test_acquire_strips_bucket_from_uri() {
        let minio = MinioInstance::start().await;
        minio.upload("dists/private/InRelease", b"release file").await;

        let mut transport = Transport::new();
        transport.configure(&make_config(&minio.endpoint)).unwrap();

        let out_file = tempfile::NamedTempFile::new().unwrap();
        let mut output = Vec::new();

        transport
            .acquire(
                "r2://apt-private/dists/private/InRelease",
                out_file.path().to_str().unwrap(),
                &mut output,
            )
            .await
            .unwrap();

        let response = String::from_utf8(output).unwrap();
        assert!(response.contains("201 URI Done"));
        assert_eq!(std::fs::read(out_file.path()).unwrap(), b"release file");
    }
}
