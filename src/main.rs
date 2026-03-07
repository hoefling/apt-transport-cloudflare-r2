mod message;
mod transport;

use anyhow::Result;
use transport::Transport;

#[tokio::main]
async fn main() -> Result<()> {
    Transport::new().run().await
}
