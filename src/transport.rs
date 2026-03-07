use anyhow::{Context, Result};
use aws_sdk_s3::Client;
use std::io::{self, BufRead, Write};

use crate::message::Message;

pub struct Transport {
    s3: Client,
    bucket: String,
}

impl Transport {
    pub async fn new() -> Result<Self> {
        let config = aws_config::from_env()
            .region(aws_config::Region::new("auto"))
            .load()
            .await;
        let s3 = Client::new(&config);
        let bucket = std::env::var("R2_BUCKET").context("R2_BUCKET env var not set")?;

        Ok(Self { s3, bucket })
    }

    pub async fn run(&self) -> Result<()> {
        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Send capabilities
        out.write_all(
            Message::format(
                100,
                "Capabilities",
                &[("Version", "1.0"), ("Single-Instance", "true")],
            )
            .as_bytes(),
        )?;
        out.flush()?;

        // Read messages from APT
        let stdin = io::stdin();
        let mut buf = String::new();
        let mut reader = stdin.lock();

        loop {
            buf.clear();
            let mut block = String::new();

            // Read until blank line (end of message)
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf)?;
                if n == 0 || buf.trim().is_empty() {
                    break;
                }
                block.push_str(&buf);
            }

            if block.is_empty() {
                continue;
            }

            if let Some(msg) = Message::parse(&block) {
                match msg.code {
                    601 => { /* configuration, ignore for now */ }
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
        // r2://bucket-name/path/to/file -> key = path/to/file
        let key = uri
            .trim_start_matches("r2://") // "apt-private/pool/main/python3-aiogram_3.24.0-1_arm64.deb"
            .split_once('/') // Some(("apt-private", "pool/main/python3-aiogram_3.24.0-1_arm64.deb"))
            .map(|(_, path)| path) // "pool/main/python3-aiogram_3.24.0-1_arm64.deb"
            .unwrap_or("")
            .to_string();

        out.write_all(Message::format(200, "URI Start", &[("URI", uri)]).as_bytes())?;
        out.flush()?;

        match self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(resp) => {
                let bytes = resp.body.collect().await?.into_bytes();
                // Write to the destination file APT requested
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
                let detail = format!("{:?}", e); // use Debug instead of Display for full error chain
                out.write_all(
                    Message::format(400, "URI Failure", &[("URI", uri), ("Message", &detail)])
                        .as_bytes(),
                )?;
            }
        }
        out.flush()?;
        Ok(())
    }
}
