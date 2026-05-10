//! Fulfillment provider selection and error handling.

use std::collections::HashSet;
use std::time::Duration;

use kino_core::{CanonicalIdentityId, CanonicalIdentityKind};

use crate::request::{Error, FulfillmentPlanDecision, Result};

/// Default number of provider failures allowed before retry is exhausted.
pub const PROVIDER_RETRY_MAX_FAILURES: u32 = 3;
/// Default delay after the first transient provider failure.
pub const PROVIDER_RETRY_INITIAL_BACKOFF: Duration = Duration::from_secs(30);
/// Maximum delay between provider retry attempts.
pub const PROVIDER_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(300);

/// A configured fulfillment provider supplied by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfiguredFulfillmentProvider<'a> {
    /// Stable provider id from configuration.
    pub id: &'a str,
    /// User preference. Higher values rank ahead of lower values.
    pub preference: i32,
    /// Declared provider capabilities.
    pub capabilities: &'a [FulfillmentProviderCapability],
}

impl<'a> ConfiguredFulfillmentProvider<'a> {
    /// Construct a configured provider descriptor.
    pub const fn new(
        id: &'a str,
        preference: i32,
        capabilities: &'a [FulfillmentProviderCapability],
    ) -> Self {
        Self {
            id,
            preference,
            capabilities,
        }
    }
}

/// Request capability a provider claims it can satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FulfillmentProviderCapability {
    /// Provider can attempt any media kind.
    AnyMedia,
    /// Provider can attempt one canonical media kind.
    MediaKind(CanonicalIdentityKind),
}

/// Error returned by a fulfillment provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FulfillmentProviderError {
    /// Provider failure may clear on a later attempt.
    Transient {
        /// Stable provider-specific error code.
        code: String,
        /// Human-readable error detail.
        message: String,
    },
    /// Provider failure should fail the request without retrying.
    Permanent {
        /// Stable provider-specific error code.
        code: String,
        /// Human-readable error detail.
        message: String,
    },
}

impl FulfillmentProviderError {
    /// Construct a transient provider error.
    pub fn transient(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Transient {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Construct a permanent provider error.
    pub fn permanent(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Permanent {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Whether retry policy applies to this error.
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }

    /// Stable provider-specific error code.
    pub fn code(&self) -> &str {
        match self {
            Self::Transient { code, .. } | Self::Permanent { code, .. } => code,
        }
    }

    /// Human-readable error detail.
    pub fn message(&self) -> &str {
        match self {
            Self::Transient { message, .. } | Self::Permanent { message, .. } => message,
        }
    }

    /// Message suitable for request status history.
    pub fn status_message(&self, provider_id: &str) -> String {
        let class = if self.is_transient() {
            "transient"
        } else {
            "permanent"
        };

        format!(
            "provider {provider_id} returned {class} error {}: {}",
            self.code(),
            self.message()
        )
    }
}

/// Retry policy for transient provider failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRetryPolicy {
    /// Number of failures allowed before the request is failed.
    pub max_failures: u32,
    /// Delay after the first transient failure.
    pub initial_backoff: Duration,
    /// Maximum retry delay.
    pub max_backoff: Duration,
}

impl ProviderRetryPolicy {
    /// Construct a retry policy.
    pub const fn new(max_failures: u32, initial_backoff: Duration, max_backoff: Duration) -> Self {
        Self {
            max_failures,
            initial_backoff,
            max_backoff,
        }
    }

    /// Return the delay for `failure_count`, or `None` when retries are exhausted.
    pub fn retry_after(self, failure_count: u32) -> Option<Duration> {
        if failure_count == 0 || failure_count >= self.max_failures {
            return None;
        }

        let exponent = failure_count.saturating_sub(1);
        let multiplier = 2_u32.saturating_pow(exponent);
        Some(
            self.initial_backoff
                .saturating_mul(multiplier)
                .min(self.max_backoff),
        )
    }
}

impl Default for ProviderRetryPolicy {
    fn default() -> Self {
        Self {
            max_failures: PROVIDER_RETRY_MAX_FAILURES,
            initial_backoff: PROVIDER_RETRY_INITIAL_BACKOFF,
            max_backoff: PROVIDER_RETRY_MAX_BACKOFF,
        }
    }
}

impl FulfillmentProviderCapability {
    fn matches(self, kind: CanonicalIdentityKind) -> bool {
        match self {
            Self::AnyMedia => true,
            Self::MediaKind(media_kind) => media_kind == kind,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::AnyMedia => 0,
            Self::MediaKind(_) => 1,
        }
    }
}

/// Selection inputs for one provider-planning pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSelectionContext<'a> {
    /// Resolved canonical media identity for the request.
    pub canonical_identity_id: CanonicalIdentityId,
    /// Providers already tried for this request and skipped during fallback.
    pub rejected_provider_ids: &'a [&'a str],
}

impl<'a> ProviderSelectionContext<'a> {
    /// Construct a provider selection context for a resolved request.
    pub const fn new(canonical_identity_id: CanonicalIdentityId) -> Self {
        Self {
            canonical_identity_id,
            rejected_provider_ids: &[],
        }
    }

    /// Exclude providers already attempted by earlier fulfillment passes.
    pub const fn with_rejected_provider_ids(self, rejected_provider_ids: &'a [&'a str]) -> Self {
        Self {
            rejected_provider_ids,
            ..self
        }
    }
}

/// A provider after ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedFulfillmentProvider {
    /// One-based rank after filtering and sorting.
    pub rank: u32,
    /// Stable provider id.
    pub provider_id: String,
    /// User preference copied from provider configuration.
    pub preference: i32,
    /// Capability that caused the provider to match.
    pub matched_capability: FulfillmentProviderCapability,
}

/// Provider-selection outcome ready to persist as a fulfillment plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSelectionPlan {
    /// Plan decision implied by the selection.
    pub decision: FulfillmentPlanDecision,
    /// Selected provider id, absent when no provider matches.
    pub selected_provider_id: Option<String>,
    /// Matching providers after fallback exclusions, ordered by rank.
    pub ranked_providers: Vec<RankedFulfillmentProvider>,
    /// Human-readable summary for the persisted fulfillment plan.
    pub summary: String,
}

/// Rank configured providers and produce the provider-selection plan.
pub fn select_fulfillment_provider(
    context: ProviderSelectionContext<'_>,
    providers: &[ConfiguredFulfillmentProvider<'_>],
) -> Result<ProviderSelectionPlan> {
    let rejected = validate_rejected_provider_ids(context.rejected_provider_ids)?;
    validate_configured_providers(providers)?;

    let media_kind = context.canonical_identity_id.kind();
    let mut ranked = providers
        .iter()
        .filter_map(|provider| {
            let provider_id = provider.id.trim();
            if rejected.contains(provider_id) {
                return None;
            }

            best_capability_match(provider.capabilities, media_kind).map(|matched_capability| {
                RankedFulfillmentProvider {
                    rank: 0,
                    provider_id: provider_id.to_owned(),
                    preference: provider.preference,
                    matched_capability,
                }
            })
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| {
        right
            .matched_capability
            .rank()
            .cmp(&left.matched_capability.rank())
            .then_with(|| right.preference.cmp(&left.preference))
            .then_with(|| left.provider_id.cmp(&right.provider_id))
    });

    for (index, provider) in ranked.iter_mut().enumerate() {
        provider.rank = rank_from_index(index);
    }

    let selected_provider_id = ranked.first().map(|provider| provider.provider_id.clone());
    let (decision, summary) = match selected_provider_id.as_deref() {
        Some(provider_id) => (
            FulfillmentPlanDecision::NeedsProvider,
            format!(
                "selected provider {provider_id} for {}",
                context.canonical_identity_id
            ),
        ),
        None if rejected.is_empty() => (
            FulfillmentPlanDecision::NeedsUserInput,
            format!(
                "no configured provider can satisfy {}",
                context.canonical_identity_id
            ),
        ),
        None => (
            FulfillmentPlanDecision::NeedsUserInput,
            format!(
                "no remaining configured provider can satisfy {}",
                context.canonical_identity_id
            ),
        ),
    };

    Ok(ProviderSelectionPlan {
        decision,
        selected_provider_id,
        ranked_providers: ranked,
        summary,
    })
}

fn best_capability_match(
    capabilities: &[FulfillmentProviderCapability],
    media_kind: CanonicalIdentityKind,
) -> Option<FulfillmentProviderCapability> {
    capabilities
        .iter()
        .copied()
        .filter(|capability| capability.matches(media_kind))
        .max_by_key(|capability| capability.rank())
}

fn validate_configured_providers(providers: &[ConfiguredFulfillmentProvider<'_>]) -> Result<()> {
    let mut seen = HashSet::with_capacity(providers.len());
    for provider in providers {
        let provider_id = validate_provider_id(provider.id)?;
        if !seen.insert(provider_id.to_owned()) {
            return Err(Error::DuplicateFulfillmentProvider {
                provider_id: provider_id.to_owned(),
            });
        }
        if provider.capabilities.is_empty() {
            return Err(Error::InvalidFulfillmentProvider {
                provider_id: provider_id.to_owned(),
                reason: "capabilities are empty",
            });
        }
    }

    Ok(())
}

fn validate_rejected_provider_ids<'a>(provider_ids: &'a [&'a str]) -> Result<HashSet<&'a str>> {
    let mut rejected = HashSet::with_capacity(provider_ids.len());
    for provider_id in provider_ids {
        rejected.insert(validate_provider_id(provider_id)?);
    }

    Ok(rejected)
}

fn validate_provider_id(provider_id: &str) -> Result<&str> {
    let trimmed = provider_id.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidFulfillmentProvider {
            provider_id: provider_id.to_owned(),
            reason: "id is empty",
        });
    }

    Ok(trimmed)
}

fn rank_from_index(index: usize) -> u32 {
    if index >= u32::MAX as usize {
        u32::MAX
    } else {
        index as u32 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::TmdbId;
    use std::time::Duration;

    const ANY: &[FulfillmentProviderCapability] = &[FulfillmentProviderCapability::AnyMedia];
    const MOVIE: &[FulfillmentProviderCapability] = &[FulfillmentProviderCapability::MediaKind(
        CanonicalIdentityKind::Movie,
    )];
    const TV: &[FulfillmentProviderCapability] = &[FulfillmentProviderCapability::MediaKind(
        CanonicalIdentityKind::TvSeries,
    )];

    #[test]
    fn single_matching_provider_is_selected() {
        let identity = movie_identity(550);
        let providers = [
            ConfiguredFulfillmentProvider::new("tv-only", 100, TV),
            ConfiguredFulfillmentProvider::new("movie-provider", 0, MOVIE),
        ];

        let plan = select_fulfillment_provider(ProviderSelectionContext::new(identity), &providers)
            .expect("provider selection should succeed");

        assert_eq!(plan.decision, FulfillmentPlanDecision::NeedsProvider);
        assert_eq!(plan.selected_provider_id.as_deref(), Some("movie-provider"));
        assert_eq!(plan.ranked_providers.len(), 1);
        assert_eq!(plan.ranked_providers[0].rank, 1);
        assert_eq!(plan.ranked_providers[0].provider_id, "movie-provider");
    }

    #[test]
    fn multiple_matching_providers_use_documented_ranking() {
        let identity = movie_identity(550);
        let providers = [
            ConfiguredFulfillmentProvider::new("z-generic-high", 100, ANY),
            ConfiguredFulfillmentProvider::new("b-movie-low", 1, MOVIE),
            ConfiguredFulfillmentProvider::new("a-movie-high", 10, MOVIE),
            ConfiguredFulfillmentProvider::new("c-movie-high", 10, MOVIE),
        ];

        let plan = select_fulfillment_provider(ProviderSelectionContext::new(identity), &providers)
            .expect("provider selection should succeed");

        let ranked_ids = plan
            .ranked_providers
            .iter()
            .map(|provider| provider.provider_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            ranked_ids,
            vec![
                "a-movie-high",
                "c-movie-high",
                "b-movie-low",
                "z-generic-high"
            ]
        );
        assert_eq!(plan.selected_provider_id.as_deref(), Some("a-movie-high"));
    }

    #[test]
    fn rejected_provider_falls_back_to_next_match() {
        let identity = movie_identity(550);
        let providers = [
            ConfiguredFulfillmentProvider::new("first", 10, MOVIE),
            ConfiguredFulfillmentProvider::new("second", 5, MOVIE),
        ];
        let rejected = ["first"];

        let plan = select_fulfillment_provider(
            ProviderSelectionContext::new(identity).with_rejected_provider_ids(&rejected),
            &providers,
        )
        .expect("provider selection should succeed");

        assert_eq!(plan.decision, FulfillmentPlanDecision::NeedsProvider);
        assert_eq!(plan.selected_provider_id.as_deref(), Some("second"));
        assert_eq!(plan.ranked_providers.len(), 1);
    }

    #[test]
    fn no_matching_provider_needs_user_input() {
        let identity = movie_identity(550);
        let providers = [ConfiguredFulfillmentProvider::new("tv-only", 100, TV)];

        let plan = select_fulfillment_provider(ProviderSelectionContext::new(identity), &providers)
            .expect("provider selection should succeed");

        assert_eq!(plan.decision, FulfillmentPlanDecision::NeedsUserInput);
        assert_eq!(plan.selected_provider_id, None);
        assert!(plan.ranked_providers.is_empty());
    }

    #[test]
    fn invalid_provider_configuration_is_rejected() {
        let identity = movie_identity(550);
        let providers = [
            ConfiguredFulfillmentProvider::new("duplicate", 0, ANY),
            ConfiguredFulfillmentProvider::new(" duplicate ", 1, ANY),
        ];

        let err = select_fulfillment_provider(ProviderSelectionContext::new(identity), &providers)
            .expect_err("duplicate provider ids should fail");

        assert!(matches!(
            err,
            Error::DuplicateFulfillmentProvider { provider_id } if provider_id == "duplicate"
        ));
    }

    #[test]
    fn provider_error_records_transient_or_permanent_class() {
        let transient = FulfillmentProviderError::transient("timeout", "provider timed out");
        let permanent = FulfillmentProviderError::permanent("not_found", "provider rejected id");

        assert!(transient.is_transient());
        assert!(!permanent.is_transient());
        assert_eq!(transient.code(), "timeout");
        assert_eq!(permanent.message(), "provider rejected id");
        assert_eq!(
            transient.status_message("watch-folder"),
            "provider watch-folder returned transient error timeout: provider timed out"
        );
    }

    #[test]
    fn retry_policy_uses_capped_exponential_backoff() {
        let policy = ProviderRetryPolicy::new(4, Duration::from_secs(5), Duration::from_secs(12));

        assert_eq!(policy.retry_after(0), None);
        assert_eq!(policy.retry_after(1), Some(Duration::from_secs(5)));
        assert_eq!(policy.retry_after(2), Some(Duration::from_secs(10)));
        assert_eq!(policy.retry_after(3), Some(Duration::from_secs(12)));
        assert_eq!(policy.retry_after(4), None);
    }

    fn movie_identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id must be positive"),
        }
    }
}
