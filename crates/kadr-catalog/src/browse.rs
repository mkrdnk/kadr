use super::*;

/// Trakt's first trending movie and show pages, in provider order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowseCatalog {
    pub movies: Vec<Media>,
    pub shows: Vec<Media>,
}

impl Client {
    /// On demand only: no crawling, pagination, recommendations or demo fallback.
    pub async fn browse(&self, cancel: &CancellationToken) -> Result<BrowseCatalog, Error> {
        self.ready(cancel)?;
        let (movies, shows) = self.lists("trending", None, cancel).await?;
        Ok(BrowseCatalog { movies, shows })
    }

    pub(super) async fn lists(
        &self,
        operation: &str,
        query: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<(Vec<Media>, Vec<Media>), Error> {
        let (movies, shows) = tokio::join!(
            self.list(MediaKind::Movie, operation, query, cancel),
            self.list(MediaKind::Series, operation, query, cancel),
        );
        // Cancellation must not become partial success from the completed endpoint.
        self.ready(cancel)?;
        match (movies, shows) {
            (Ok(movies), Ok(shows)) => Ok((movies, shows)),
            (Ok(movies), Err(_)) if !movies.is_empty() => Ok((movies, Vec::new())),
            (Err(_), Ok(shows)) if !shows.is_empty() => Ok((Vec::new(), shows)),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    async fn list(
        &self,
        kind: MediaKind,
        operation: &str,
        query: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Media>, Error> {
        let url = trakt::list_url(kind, operation, query)?;
        trakt::list(&self.json(url, cancel).await?, kind)
    }
}
