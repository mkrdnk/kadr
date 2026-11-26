use kadr_core::torrent::*;

fn ep(season: u16, episode: u16) -> EpisodeNumber {
    EpisodeNumber::new(season, episode).unwrap()
}
fn video(index: u32, path: &str) -> VideoCandidate {
    VideoCandidate::from_file(
        FileIndex(index),
        TorrentRelativePath::new(path).unwrap(),
        123,
    )
    .unwrap()
}

#[test]
fn reject_traversal_absolute_and_nonportable_paths() {
    for path in [
        "",
        "/",
        "/movie.mkv",
        ".",
        "..",
        "a/../movie.mkv",
        "a/./movie.mkv",
        "a//movie.mkv",
        "a/",
        "../movie.mkv",
        r"..\movie.mkv",
        r"C:\movie.mkv",
        "C:movie.mkv",
        r"\\server\movie.mkv",
        "a\0.mkv",
        "a\n.mkv",
        "a/movie.mkv.",
        "a /movie.mkv",
        "NUL.mkv",
        "aux/file.mp4",
        "COM1/file",
        "LPT9.txt",
        "a/file:stream",
        "a/*.mp4",
        "a/?",
        "a/|",
        "a/<x>",
    ] {
        assert!(TorrentRelativePath::new(path).is_err(), "{path:?}");
    }
}

#[test]
fn safe_relative_paths_are_literal_not_url_decoded() {
    for path in [
        "Series/Season 01/épisode.S01E02.mkv",
        "a/%2e%2e/movie.mp4",
        "com10/movie.mp4",
        "...name.mp4",
    ] {
        let validated = TorrentRelativePath::new(path).unwrap();
        assert_eq!(validated.as_str(), path);
        assert!(!format!("{validated:?}").contains(path));
    }
}

#[test]
fn common_episode_names_case_specials_and_boundaries() {
    for (name, number) in [
        ("Show.S01E02.1080p.mkv", ep(1, 2)),
        ("Show.1x02.mkv", ep(1, 2)),
        ("s00e01.MP4", ep(0, 1)),
        ("Show-s12e003.mkv", ep(12, 3)),
        ("[S01E02].mkv", ep(1, 2)),
    ] {
        assert_eq!(
            recognize_episode(name),
            EpisodeRecognition::Recognized(number),
            "{name}"
        );
    }
    for name in [
        "movie.2026.mkv",
        "ShowS01E02.mkv",
        "a1x02b.mkv",
        "1280x720.mkv",
        "special.mkv",
    ] {
        assert_eq!(
            recognize_episode(name),
            EpisodeRecognition::Unrecognized,
            "{name}"
        );
    }
}

#[test]
fn multiple_markers_chains_ranges_and_invalid_numbers_are_explicitly_ambiguous() {
    for name in [
        "Show.S01E02.S01E03.mkv",
        "Show.S01E02E03.mkv",
        "Show.S01E02-E04.mkv",
        "Show.S01E02 - E04.mkv",
        "Show.S01E02 E03.mkv",
        "Show.1x02-03.mkv",
        "Show.S01E02.S01E02.mkv",
        "Show.S01E00.mkv",
        "Show.S999999E02.mkv",
        "Show.S01E999999.mkv",
    ] {
        assert!(
            matches!(recognize_episode(name), EpisodeRecognition::Ambiguous(_)),
            "{name}"
        );
    }
    assert_eq!(
        recognize_episode("S01E02E03.mkv"),
        EpisodeRecognition::Ambiguous(vec![ep(1, 2), ep(1, 3)])
    );
}

#[test]
fn video_filter_uses_basename_extension_and_never_parent_episode() {
    for path in [
        "show.S01E02.srt",
        "show.S01E02.jpg",
        "show.mkv.exe",
        "show",
        "show.nfo",
    ] {
        assert!(
            VideoCandidate::from_file(FileIndex(0), TorrentRelativePath::new(path).unwrap(), 1)
                .is_none()
        );
    }
    let candidate = video(42, "S01E02/movie.MKV");
    assert_eq!(candidate.episode(), &EpisodeRecognition::Unrecognized);
    assert_eq!(candidate.size(), 123);
    assert_eq!(candidate.index(), FileIndex(42));
    assert!(!format!("{candidate:?}").contains("movie.MKV"));
}

#[test]
fn ambiguity_and_unknown_names_remain_manually_selectable() {
    let mut selection = VideoSelection::new(vec![
        video(10, "Show.S01E02.mkv"),
        video(20, "Show.S01E02.720p.mp4"),
        video(30, "unknown.mkv"),
        video(40, "Show.S01E03E04.mkv"),
    ])
    .unwrap();
    assert_eq!(selection.selected(), None);
    assert_eq!(
        selection.match_episode(ep(1, 2)),
        EpisodeMatch::Ambiguous(vec![FileIndex(10), FileIndex(20)])
    );
    assert_eq!(
        selection.match_episode(ep(1, 3)),
        EpisodeMatch::Ambiguous(vec![FileIndex(40)])
    );
    selection.select(FileIndex(30)).unwrap();
    assert_eq!(selection.match_episode(ep(1, 99)), EpisodeMatch::NotFound);
    assert_eq!(selection.selected(), Some(FileIndex(30)));
    assert_eq!(
        selection.select(FileIndex(999)),
        Err(SelectionError::UnknownFileIndex)
    );
    assert_eq!(selection.selected(), Some(FileIndex(30)));
    selection.select(FileIndex(40)).unwrap();
    assert_eq!(selection.selected(), Some(FileIndex(40)));
}

#[test]
fn unique_match_is_advisory_and_duplicate_indices_are_rejected() {
    let candidate = video(3, "S01E02.mkv");
    let selection = VideoSelection::new(vec![candidate.clone()]).unwrap();
    assert_eq!(
        selection.match_episode(ep(1, 2)),
        EpisodeMatch::Unique(FileIndex(3))
    );
    assert_eq!(selection.selected(), None);
    assert_eq!(
        VideoSelection::new(vec![candidate.clone(), candidate]).unwrap_err(),
        SelectionError::DuplicateFileIndex
    );
}
