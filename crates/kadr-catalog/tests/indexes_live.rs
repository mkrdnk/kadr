//! Explicit metadata-only probes. Never open a magnet or contact peers.
use kadr_catalog::{
    CancellationToken,
    popcorn::{Client, Index},
};
use kadr_core::{
    catalog::{ExternalMetadata, MediaKind},
    sources::MediaQuery,
    torrent::EpisodeNumber,
};

async fn lookup(index: Index, title: &str, year: u16, imdb: &str, episode: Option<EpisodeNumber>) {
    let q = MediaQuery {
        media_id: "probe:catalog".into(),
        kind: if episode.is_some() {
            MediaKind::Series
        } else {
            MediaKind::Movie
        },
        title: title.into(),
        original_title: None,
        year: Some(year),
        external: ExternalMetadata {
            imdb_id: Some(imdb.into()),
            ..Default::default()
        },
        episode,
    };
    let rows = Client::new(None)
        .unwrap()
        .search_index(index, &q, 0, &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        !rows.is_empty(),
        "no eligible releases for {title} from {}",
        index.name()
    );
    assert!(
        rows.iter()
            .all(|row| row.episode == episode && index.accepts(row))
    );
    println!(
        "{}: {title}, {} eligible rows ({} with file mapping); no peer requests",
        index.name(),
        rows.len(),
        rows.iter()
            .filter(|r| r.episode_file_hint.is_some())
            .count()
    );
}

#[tokio::test]
#[ignore = "contacts the public movie index"]
async fn yts_movie() {
    lookup(Index::Yts, "The Matrix", 1999, "tt0133093", None).await;
}

#[tokio::test]
#[ignore = "contacts the public show index"]
async fn anime_episode() {
    lookup(
        Index::Popcorn,
        "Cowboy Bebop",
        1998,
        "tt0213338",
        EpisodeNumber::new(1, 1),
    )
    .await;
}

#[tokio::test]
#[ignore = "contacts the public show index"]
async fn series_episode() {
    lookup(
        Index::Popcorn,
        "The Simpsons",
        1989,
        "tt0096697",
        EpisodeNumber::new(1, 1),
    )
    .await;
}
