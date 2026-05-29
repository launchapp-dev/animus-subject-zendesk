use anyhow::Result;

pub const ENV_SUBDOMAIN: &str = "ZENDESK_SUBDOMAIN";
pub const ENV_BASE_URL: &str = "ZENDESK_BASE_URL";
pub const ENV_EMAIL: &str = "ZENDESK_EMAIL";
pub const ENV_API_TOKEN: &str = "ZENDESK_API_TOKEN";
pub const ENV_QUERY: &str = "ZENDESK_QUERY";

#[derive(Debug, Clone)]
pub struct ZendeskConfig {
    pub base_url: String,
    pub email: Option<String>,
    pub api_token: Option<String>,
    pub query: Option<String>,
}

impl ZendeskConfig {
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var(ENV_BASE_URL)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim_end_matches('/').to_string())
            .or_else(|| {
                std::env::var(ENV_SUBDOMAIN)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(|subdomain| format!("https://{}.zendesk.com", subdomain.trim()))
            })
            .unwrap_or_default();

        let email = std::env::var(ENV_EMAIL).ok().filter(|s| !s.is_empty());
        let api_token = std::env::var(ENV_API_TOKEN).ok().filter(|s| !s.is_empty());
        let query = std::env::var(ENV_QUERY)
            .ok()
            .filter(|s| !s.trim().is_empty());

        Ok(Self {
            base_url,
            email,
            api_token,
            query,
        })
    }

    pub fn for_testing(api_base: impl Into<String>) -> Self {
        Self {
            base_url: api_base.into(),
            email: Some("agent@example.com".into()),
            api_token: Some("test-token".into()),
            query: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testing_config_uses_base_url() {
        let config = ZendeskConfig::for_testing("https://example.zendesk.com");
        assert_eq!(config.base_url, "https://example.zendesk.com");
        assert_eq!(config.email.as_deref(), Some("agent@example.com"));
    }
}
