use kadr_catalog::{CancellationToken, rutracker};
use kadr_core::{catalog::MediaKind, sources::MediaQuery};

/// Anonymous, read-only probe for a freely distributed film. Never opens a
/// topic, accesses the system vault, downloads metainfo or starts playback.
#[tokio::test]
#[ignore = "contacts the public RuTracker search page"]
async fn anonymous_search_reports_hits_or_a_login_requirement() {
    let query = MediaQuery {
        media_id: "live:big-buck-bunny".into(),
        kind: MediaKind::Movie,
        title: "Big Buck Bunny".into(),
        original_title: None,
        year: Some(2008),
        episode: None,
        external: Default::default(),
    };
    let result = rutracker::Client::new()
        .unwrap()
        .search_outcome(&query, &CancellationToken::new())
        .await;
    match result {
        Err(rutracker::Error::AuthenticationRequired) => {
            println!("Login required for search; no release list was verified.");
        }
        Ok(outcome) => {
            assert!(
                !outcome.offers.is_empty(),
                "No selectable releases for the control title (had_topics={})",
                outcome.had_topics
            );
            println!(
                "Selectable releases for the control title: {}",
                outcome.offers.len()
            );
        }
        Err(error) => panic!("Public RuTracker search failed: {error}"),
    }
}

/// Probe only the topic HTML; never print a magnet or start a torrent session.
#[tokio::test]
#[ignore = "requires an explicit KADR_RUTRACKER_TEST_TOPIC and public network access"]
async fn topic_attachment_status() {
    let id = std::env::var("KADR_RUTRACKER_TEST_TOPIC")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let result = rutracker::Client::new()
        .unwrap()
        .magnet(id, &CancellationToken::new())
        .await;
    match result {
        Ok(Some(_)) => println!("Topic readable: found a validated attachment magnet."),
        Ok(None) => println!("Topic readable: no attachment magnet."),
        Err(rutracker::Error::Http(403)) => {
            println!("HTTP 403: topic inaccessible; magnet not checked.")
        }
        Err(rutracker::Error::AuthenticationRequired) => {
            println!("Login required; magnet not checked.")
        }
        Err(error) => panic!("Topic probe failed: {error}"),
    }
}
