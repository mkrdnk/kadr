//! Headless smoke: `player <mpv|vlc> <path-or-url> [resume-seconds] [--headless]`.
#[cfg(unix)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use std::{io, time::Duration};

    use kadr_media::players::{PlayRequest, PlayerKind, PlayerOptions, PlayerSession};
    use tokio_util::sync::CancellationToken;

    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: player <mpv|vlc> <path-or-url> [resume-seconds] [--headless]",
        )
    };
    let mut args = std::env::args_os().skip(1);
    let kind = match args.next().as_deref().and_then(|arg| arg.to_str()) {
        Some("mpv") => PlayerKind::Mpv,
        Some("vlc") => PlayerKind::Vlc,
        _ => return Err(invalid()),
    };
    let media = args.next().ok_or_else(invalid)?;
    let mut resume = None;
    let mut options = PlayerOptions::for_player(kind);
    for arg in args {
        if arg == "--headless" && !options.headless {
            options.headless = true;
        } else if resume.is_none() {
            let seconds = arg
                .to_str()
                .and_then(|arg| arg.parse::<f64>().ok())
                .ok_or_else(invalid)?;
            resume = Some(Duration::try_from_secs_f64(seconds).map_err(|_| invalid())?);
        } else {
            return Err(invalid());
        }
    }
    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancel.cancel();
        }
    });
    let result = async {
        let mut session =
            PlayerSession::launch(kind, PlayRequest { media, resume }, options, &cancel).await?;
        let monitor = async {
            loop {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                if let Some(status) = session.try_wait()? {
                    eprintln!("Player exited: {status}");
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(io::Error::other("player exited unsuccessfully"))
                    };
                }
                let snapshot = session.snapshot(&cancel).await?;
                eprintln!(
                    "Position: {:?}; duration: {:?}",
                    snapshot.position, snapshot.duration
                );
                tokio::select! {
                    () = cancel.cancelled() => return Ok(()),
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
        .await;
        let stopped = session.stop().await;
        if cancel.is_cancelled() {
            stopped.map(|_| ())
        } else {
            monitor.and(stopped.map(|_| ()))
        }
    }
    .await;
    signal.abort();
    result
}

#[cfg(not(unix))]
fn main() {
    eprintln!("This player spike supports Unix sockets on Linux/macOS only.");
    std::process::exit(1);
}
