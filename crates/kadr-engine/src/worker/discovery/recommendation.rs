//! Recommendation is an IO-free decision, never permission to prepare media.
//! The explicit watch command must repeat it against current permissions and
//! inspect the prepared file list before starting peers or opening a player.

use super::*;

impl Discovery {
    pub(super) fn recommendation(
        &self,
        preferences: &crate::PlaybackPreferences,
        allowed: impl Fn(&MediaSource) -> bool,
    ) -> Option<usize> {
        if crate::playback::validate_preferences(preferences).is_err() {
            return None;
        }
        let state = self.current.as_ref()?;
        if state.pending != 0
            || (state.query.kind == MediaKind::Series && state.query.episode.is_none())
        {
            return None;
        }
        let resolution = preferences
            .preferred_resolution
            .as_deref()
            .and_then(crate::playback::preferred_height)
            .or_else(|| self.rank_preferences.resolutions.first().copied());
        let language = preferences
            .preferred_language
            .as_deref()
            .or_else(|| self.rank_preferences.languages.first().map(String::as_str));
        // Preferences are a request, not a guarantee about uninspected tracks.
        // A declared match beats a known mismatch; unknown metadata comes last.
        // With no request retain the source list's deterministic seed-first order.
        state
            .ranked
            .iter()
            .enumerate()
            .filter_map(|(position, (handle, source))| {
                let source = recommendation_source(state, source)?;
                allowed(source).then_some((position, *handle, source))
            })
            .min_by_key(|(position, _, source)| {
                let quality = resolution.map_or(0, |wanted| match source.resolution {
                    Some(actual) if actual == wanted => 0,
                    Some(_) => 1,
                    None => 2,
                });
                let language =
                    language.map_or(0, |wanted| language_distance(wanted, &source.languages));
                (quality, language, *position)
            })
            .map(|(_, handle, _)| handle)
    }
}

fn language_distance(wanted: &str, actual: &[String]) -> u8 {
    if actual.iter().any(|tag| tag.eq_ignore_ascii_case(wanted)) {
        return 0;
    }
    let base = crate::playback::preferred_audio_language(wanted);
    if base.is_some()
        && actual
            .iter()
            .any(|tag| crate::playback::preferred_audio_language(tag) == base)
    {
        return 1;
    }
    if actual.is_empty() { 3 } else { 2 }
}

fn recommendation_source<'a>(
    state: &'a SearchState,
    source: &'a MediaSource,
) -> Option<&'a MediaSource> {
    if matches!(source.locator, Locator::Http(_)) {
        // Dedup can select a manual display representative or merge its
        // origins into the Commons row. Neither is provider authority. Recheck
        // the complete original trusted offer for exactly the same locator.
        state.batches.iter().find(|original| {
            original.locator == source.locator
                && original.provider == kadr_catalog::commons::ID
                && eligible(&state.query, original)
        })
    } else {
        eligible(&state.query, source).then_some(source)
    }
}

fn eligible(query: &MediaQuery, source: &MediaSource) -> bool {
    if source.possible_match
        || source.kind != query.kind
        || source.size == Some(0)
        || matches!(source.provider.as_str(), "manual-http" | "manual-torrent")
        || source.clone().normalize().is_err()
    {
        return false;
    }
    let exact = if source.provider == "rutracker" {
        matches!(source.locator, Locator::RuTracker(_))
            && rutracker::classify_release(&source.title, query)
                == Some(rutracker::ReleaseMatch::Exact)
    } else {
        kadr_core::sources::matches_query(query, source)
    };
    if !exact {
        return false;
    }
    match &source.locator {
        // Commons supplies reviewed fixed-host identities and an available
        // encoding from its API. A generic HTTP URL or a seed claim is not
        // equivalent evidence. No HEAD, Range, or media request happens here.
        Locator::Http(_) => kadr_catalog::commons::accepts(source),
        Locator::RuTracker(_) | Locator::Magnet(_) | Locator::TorrentUrl(_) => {
            source.seeds.is_some_and(|seeds| seeds > 0)
        }
        Locator::TorrentFile(_) => false,
    }
}

impl Worker {
    fn current_recommendation(&self, preferences: &crate::PlaybackPreferences) -> Option<usize> {
        if self.tracker_search.is_cancelled() {
            return None;
        }
        let state = self.discovery.current.as_ref()?;
        let query = MediaQuery::from_media(self.media.as_ref()?, self.catalog_episode().as_ref())?;
        if state.query != query {
            return None;
        }
        let providers = self.registry(&query).available(&query);
        self.discovery.recommendation(preferences, |source| {
            providers.iter().any(|provider| {
                provider.id() == source.provider
                    && provider.validate().is_ok()
                    && provider.accepts(source)
            })
        })
    }

    pub(in crate::worker) fn recommend_playback(
        &mut self,
        preferences: crate::PlaybackPreferences,
    ) {
        let index = self.current_recommendation(&preferences);
        self.discovery.playback_request = Some(preferences);
        self.send(Event::PlaybackRecommendation {
            stamp: crate::DiscoveryStamp(self.tracker_search.clone()),
            index,
        });
    }

    /// An old handle, changed target, cancelled search, changed preferences or
    /// revoked provider permission cannot authorize preparation.
    pub(in crate::worker) fn validate_watch_recommendation(
        &self,
        index: usize,
        preferences: &crate::PlaybackPreferences,
    ) -> bool {
        self.discovery.playback_request.as_ref() == Some(preferences)
            && self.current_recommendation(preferences) == Some(index)
    }

    pub(in crate::worker) fn validate_legacy_watch_recommendation(&self, index: usize) -> bool {
        self.current_recommendation(
            self.discovery
                .playback_request
                .as_ref()
                .unwrap_or(&crate::PlaybackPreferences::default()),
        ) == Some(index)
    }

    /// Invoke after explicit preparation and before Play. Metainfo, not a
    /// provider's episode label, determines which file can be started safely.
    pub(in crate::worker) fn recommended_selection_is_unambiguous(&self, index: usize) -> bool {
        let Some(state) = &self.discovery.current else {
            return false;
        };
        let Some((_, offer)) = state.ranked.iter().find(|(handle, _)| *handle == index) else {
            return false;
        };
        let Some(selection) = &self.selection else {
            return false;
        };
        let Some(prepared) = selection.origin.prepared() else {
            return matches!(offer.locator, Locator::Http(_));
        };
        let Some(file_id) = selection.file_id else {
            return false;
        };
        if let Some(hint) = offer.episode_file_hint.as_deref() {
            return popcorn::unique_video_hint(&prepared.files, hint, state.query.episode)
                == Some(file_id);
        }
        unambiguous_file(&self.file_choices(), file_id, state.query.episode)
    }
}

fn unambiguous_file(
    choices: &[crate::FileChoice],
    file_id: usize,
    episode: Option<kadr_core::torrent::EpisodeNumber>,
) -> bool {
    if !choices
        .iter()
        .any(|choice| choice.index == file_id && choice.len > 0)
    {
        return false;
    }
    match episode {
        Some(target) => {
            // Even one unique exact marker is inconclusive if another
            // video contains an ambiguous range/compound episode marker.
            let mut matches = choices.iter().filter(|choice| {
                choice.season == Some(target.season()) && choice.episode == Some(target.episode())
            });
            !choices.iter().any(|choice| choice.ambiguous)
                && matches.next().is_some_and(|choice| choice.index == file_id)
                && matches.next().is_none()
        }
        None => matches!(choices, [only]
            if only.index == file_id && only.episode.is_none() && !only.ambiguous),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode_choice(
        index: usize,
        season: Option<u16>,
        episode: Option<u16>,
        ambiguous: bool,
    ) -> crate::FileChoice {
        crate::FileChoice {
            index,
            name: "video.mkv".into(),
            len: 1024,
            season,
            episode,
            ambiguous,
        }
    }

    #[test]
    fn exact_episode_file_requires_unique_match_and_no_ambiguous_video() {
        let target = kadr_core::torrent::EpisodeNumber::new(2, 3);
        let exact = episode_choice(7, Some(2), Some(3), false);
        assert!(unambiguous_file(std::slice::from_ref(&exact), 7, target));
        assert!(!unambiguous_file(std::slice::from_ref(&exact), 8, target));
        assert!(!unambiguous_file(
            &[
                episode_choice(7, Some(2), Some(3), false),
                episode_choice(8, Some(2), Some(3), false),
            ],
            7,
            target,
        ));
        assert!(!unambiguous_file(
            &[
                episode_choice(7, Some(2), Some(3), false),
                episode_choice(8, None, None, true),
            ],
            7,
            target,
        ));
        assert!(!unambiguous_file(
            &[episode_choice(7, None, None, false)],
            7,
            target,
        ));
        assert!(unambiguous_file(
            &[exact, episode_choice(8, Some(2), Some(4), false)],
            7,
            target,
        ));
    }

    #[test]
    fn movie_file_must_be_one_non_episodic_video() {
        let movie = episode_choice(1, None, None, false);
        assert!(unambiguous_file(std::slice::from_ref(&movie), 1, None));
        assert!(!unambiguous_file(&[], 1, None));
        assert!(!unambiguous_file(
            &[episode_choice(1, None, None, false), movie],
            1,
            None,
        ));
        assert!(!unambiguous_file(
            &[episode_choice(1, Some(1), Some(2), false)],
            1,
            None,
        ));
        assert!(!unambiguous_file(
            &[episode_choice(1, None, None, true)],
            1,
            None,
        ));
        let mut empty = episode_choice(1, None, None, false);
        empty.len = 0;
        assert!(!unambiguous_file(&[empty], 1, None));
    }
}
