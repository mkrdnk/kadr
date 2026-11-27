//! Only the reviewed movie identities can trigger this public, keyless request.
use super::*;
use kadr_catalog::commons;

pub(super) struct Provider {
    pub proxy: Option<String>,
}

impl SourceProvider for Provider {
    fn id(&self) -> &str {
        commons::ID
    }

    fn name(&self) -> &str {
        "Wikimedia Commons"
    }

    fn supports(&self, query: &MediaQuery) -> bool {
        commons::supports(query)
    }

    fn validate(&self) -> Result<(), ProviderError> {
        if self
            .proxy
            .as_deref()
            .is_none_or(kadr_core::proxy::is_valid_endpoint)
        {
            Ok(())
        } else {
            Err(ProviderError::Failed(
                "Проверьте адрес прокси каталога.".into(),
            ))
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(20)
    }

    fn accepts(&self, offer: &MediaSource) -> bool {
        commons::accepts(offer)
    }

    fn search<'a>(&'a self, query: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a> {
        Box::pin(async move {
            let client = commons::Client::new(self.proxy.as_deref())
                .map_err(|error| ProviderError::Failed(error.to_string()))?;
            client
                .search(query, now(), cancel)
                .await
                .map(Into::into)
                .map_err(|error| ProviderError::Failed(error.to_string()))
        })
    }
}
