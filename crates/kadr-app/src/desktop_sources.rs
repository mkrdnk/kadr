//! Pure request bookkeeping; no engine, GPUI or IO dependencies.

pub(super) struct CardOpenRequest {
    pub(super) media_id: String,
    pub(super) navigation: u64,
}

impl CardOpenRequest {
    pub(super) fn should_prefetch(
        &self,
        media_id: Option<&str>,
        movie: bool,
        navigation: u64,
    ) -> bool {
        movie && media_id == Some(self.media_id.as_str()) && navigation == self.navigation
    }
}

#[derive(Default)]
pub(super) struct SourceRequests(u64);

impl SourceRequests {
    pub(super) fn begin(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }

    pub(super) fn current(&self) -> u64 {
        self.0
    }

    pub(super) fn is_current(&self, request: u64) -> bool {
        self.0 == request
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveryTarget {
    media_id: String,
    episode: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DiscoveryIntent {
    #[default]
    Browse,
    WatchMovie {
        navigation: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WatchDecision {
    None,
    ShowSources,
    Play(usize),
}

/// A prepared automatic release is not a manual override for the next click.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum MovieChoice {
    #[default]
    None,
    Explicit,
    Recommended {
        navigation: u64,
    },
}

impl DiscoveryIntent {
    // Consume the click once. Neither a partial result nor an ordinary Sources
    // search is authority to start a player.
    pub(super) fn finish(
        &mut self,
        navigation: u64,
        pending: usize,
        recommended: Option<usize>,
    ) -> WatchDecision {
        if pending != 0 || std::mem::take(self) != (Self::WatchMovie { navigation }) {
            return WatchDecision::None;
        }
        recommended.map_or(WatchDecision::ShowSources, WatchDecision::Play)
    }
}

/// Untagged legacy events are meaningful only during explicit preparation.
#[derive(Default)]
pub(super) struct SourcePresentation {
    discovery: Option<DiscoveryTarget>,
    pub(super) intent: DiscoveryIntent,
    pub(super) preparing: bool,
    pub(super) received_discovery: bool,
}

impl SourcePresentation {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn discover(&mut self, media_id: String, episode: Option<(u16, u16)>, retry: bool) {
        self.clear();
        self.discovery = Some(DiscoveryTarget { media_id, episode });
        // RetrySources may retry failed metadata rather than start discovery.
        self.preparing = retry;
    }

    pub(super) fn prepare(&mut self) {
        self.clear();
        self.preparing = true;
    }

    pub(super) fn discovering(&self) -> bool {
        self.discovery.is_some()
    }

    pub(super) fn accepts_discovery(&self, media_id: &str, episode: Option<(u16, u16)>) -> bool {
        self.discovery
            .as_ref()
            .is_some_and(|target| target.media_id == media_id && target.episode == episode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_current_movie_card_open_can_start_prefetch() {
        let request = CardOpenRequest {
            media_id: "movie".into(),
            navigation: 4,
        };
        assert!(request.should_prefetch(Some("movie"), true, 4));
        assert!(!request.should_prefetch(Some("movie"), false, 4));
        assert!(!request.should_prefetch(Some("other"), true, 4));
        assert!(!request.should_prefetch(None, true, 4));
        assert!(!request.should_prefetch(Some("movie"), true, 5));
    }

    #[test]
    fn discovery_identity_includes_episode_not_only_show() {
        let mut state = SourcePresentation::default();
        state.discover("show".into(), Some((1, 2)), false);
        assert!(state.accepts_discovery("show", Some((1, 2))));
        assert!(!state.accepts_discovery("show", Some((1, 1))));
        assert!(!state.accepts_discovery("show", None));
        assert!(!state.accepts_discovery("other", Some((1, 2))));
        state.discover("show".into(), Some((1, 3)), false);
        assert!(!state.accepts_discovery("show", Some((1, 2))));
        assert!(state.accepts_discovery("show", Some((1, 3))));
    }

    #[test]
    fn dismissal_navigation_manual_and_selection_discard_discovery() {
        let mut state = SourcePresentation::default();
        for prepare in [false, true] {
            state.discover("movie".into(), None, false);
            state.received_discovery = true;
            if prepare {
                state.prepare();
            } else {
                state.clear();
            }
            assert!(!state.discovering());
            assert!(!state.received_discovery);
            assert!(!state.accepts_discovery("movie", None));
            assert_eq!(state.preparing, prepare);
        }
    }

    #[test]
    fn retry_accepts_either_discovery_or_failed_metadata_preparation() {
        let mut state = SourcePresentation::default();
        state.discover("movie".into(), None, true);
        assert!(state.preparing);
        assert!(state.accepts_discovery("movie", None));
        state.prepare();
        assert!(!state.accepts_discovery("movie", None));
        assert!(state.preparing);
    }

    #[test]
    fn old_picker_results_and_errors_cannot_replace_newer_paste_or_launch() {
        let mut requests = SourceRequests::default();
        let old_picker = requests.begin();
        assert_eq!(requests.current(), old_picker);
        assert!(requests.is_current(old_picker));
        let newer_paste = requests.begin();
        assert!(!requests.is_current(old_picker));
        assert!(requests.is_current(newer_paste));
        let next_picker = requests.begin();
        requests.begin();
        assert!(!requests.is_current(next_picker));
        assert!(!requests.is_current(old_picker));
    }

    #[test]
    fn only_explicit_watch_consumes_the_final_recommendation_once() {
        let mut watch = DiscoveryIntent::WatchMovie { navigation: 4 };
        assert_eq!(watch.finish(4, 1, Some(7)), WatchDecision::None);
        assert_eq!(watch.finish(4, 0, Some(11)), WatchDecision::Play(11));
        assert_eq!(watch.finish(4, 0, Some(11)), WatchDecision::None);
        let mut watch = DiscoveryIntent::WatchMovie { navigation: 4 };
        assert_eq!(watch.finish(4, 0, None), WatchDecision::ShowSources);
        let mut watch = DiscoveryIntent::WatchMovie { navigation: 4 };
        assert_eq!(watch.finish(5, 0, Some(11)), WatchDecision::None);
        assert_eq!(
            DiscoveryIntent::Browse.finish(4, 0, Some(11)),
            WatchDecision::None
        );
    }
}
