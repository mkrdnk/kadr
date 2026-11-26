//! Deliberately accepts only private metainfo and explicit loopback peers.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, anyhow, bail};
use kadr_media::{Server, ServerOptions};
use kadr_torrent::LocalSession;
use tokio::time::timeout;

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 || args.iter().any(|a| a == "--help") {
        eprintln!(
            "Usage: local_torrent PRIVATE.torrent DEDICATED_OUTPUT_DIR FILE_ID [127.0.0.1:PORT ...]\n\
             Uses existing fixture files (overwrite/rehash enabled). No public torrents.\n\
             Prints a private bearer URL for deliberate use by a local player; Ctrl-C stops."
        );
        return Ok(());
    }
    let bytes = tokio::fs::read(&args[0])
        .await
        .map_err(|_| anyhow!("cannot read supplied .torrent file"))?;
    let output = PathBuf::from(&args[1]);
    if !output.is_dir() {
        bail!("create a dedicated output directory first");
    }
    let file_id = args[2].parse().context("file ID must be an integer")?;
    let peers = args[3..]
        .iter()
        .map(|s| s.parse())
        .collect::<Result<Vec<_>, _>>()?;
    let session = timeout(Duration::from_secs(30), LocalSession::new(output)).await??;
    let result: anyhow::Result<()> = async {
        let selected = timeout(
            Duration::from_secs(30),
            session.add_selected(bytes.into(), file_id, peers, true),
        )
        .await??;
        eprintln!("BitTorrent listener: {}", session.listen_addr());
        for file in &selected.files {
            eprintln!("file {}: {} bytes", file.id, file.len);
        }
        let source: Arc<dyn kadr_media::ByteSource> = selected.source;
        let server = Server::start(source, ServerOptions::default()).await?;
        println!("{}", server.private_url());
        tokio::signal::ctrl_c().await?;
        timeout(Duration::from_secs(3), server.shutdown()).await??;
        Ok(())
    }
    .await;
    timeout(Duration::from_secs(3), session.stop()).await?;
    result
}
