use std::{io, sync::Arc};

use kadr_media::{LocalFile, Server, ServerOptions};

/// Deliberately prints a private bearer URL to stdout for copying to a player.
#[tokio::main]
async fn main() -> io::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let path = arguments.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: http_range <local-file>",
        )
    })?;
    if arguments.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: http_range <local-file>",
        ));
    }
    // Do not propagate filesystem errors containing the personal source path.
    let source = LocalFile::open(path)
        .await
        .map_err(|error| io::Error::new(error.kind(), "cannot open the selected regular file"))?;
    let server = Server::start(Arc::new(source), ServerOptions::default()).await?;
    println!("{}", server.private_url());
    eprintln!("Private local URL printed to stdout. Press Ctrl+C to stop.");
    let signal = tokio::signal::ctrl_c().await;
    server.shutdown().await?;
    signal
}
