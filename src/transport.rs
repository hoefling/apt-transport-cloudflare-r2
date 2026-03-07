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
