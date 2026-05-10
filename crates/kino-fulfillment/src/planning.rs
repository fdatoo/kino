//! Pure fulfillment planning.

use kino_core::{CanonicalIdentityId, Request};

use crate::{
    provider::{
        ConfiguredFulfillmentProvider, ProviderSelectionContext, ProviderSelectionPlan,
        RankedFulfillmentProvider, select_fulfillment_provider,
    },
    request::{Error, FulfillmentPlanDecision, Result},
};

/// Library facts used by fulfillment planning for one resolved request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FulfillmentLibraryState {
    /// Whether the library already has a media item for the request identity.
    pub contains_resolved_identity: bool,
}

impl FulfillmentLibraryState {
    /// Construct library state for one request planning pass.
    pub const fn new(contains_resolved_identity: bool) -> Self {
        Self {
            contains_resolved_identity,
        }
    }
}

/// Inputs for deterministic fulfillment planning.
#[derive(Debug, Clone, Copy)]
pub struct FulfillmentPlanningInput<'a> {
    /// Request being planned.
    pub request: &'a Request,
    /// Current library state for the request identity.
    pub library: FulfillmentLibraryState,
    /// Configured providers available for fulfillment.
    pub providers: &'a [ConfiguredFulfillmentProvider<'a>],
    /// Providers already rejected by earlier fulfillment attempts.
    pub rejected_provider_ids: &'a [&'a str],
}

impl<'a> FulfillmentPlanningInput<'a> {
    /// Construct planning input with no rejected providers.
    pub const fn new(
        request: &'a Request,
        library: FulfillmentLibraryState,
        providers: &'a [ConfiguredFulfillmentProvider<'a>],
    ) -> Self {
        Self {
            request,
            library,
            providers,
            rejected_provider_ids: &[],
        }
    }

    /// Exclude providers already rejected by earlier fulfillment attempts.
    pub const fn with_rejected_provider_ids(self, rejected_provider_ids: &'a [&'a str]) -> Self {
        Self {
            rejected_provider_ids,
            ..self
        }
    }
}

/// Arguments passed to a provider for a fulfillment attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FulfillmentProviderArgs {
    /// Canonical identity the provider should satisfy.
    pub canonical_identity_id: CanonicalIdentityId,
}

impl FulfillmentProviderArgs {
    /// Construct provider arguments for a resolved canonical identity.
    pub const fn new(canonical_identity_id: CanonicalIdentityId) -> Self {
        Self {
            canonical_identity_id,
        }
    }
}

/// Reason planning needs user input before fulfillment can continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FulfillmentUserInputReason {
    /// No configured provider can satisfy the request identity.
    NoMatchingProvider,
    /// All matching configured providers have already been rejected.
    NoRemainingProvider,
}

/// Deterministic fulfillment plan for a resolved request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputedFulfillmentPlan {
    /// The library already contains the requested media.
    AlreadySatisfied {
        /// Resolved canonical identity.
        canonical_identity_id: CanonicalIdentityId,
        /// Human-readable summary for persistence.
        summary: String,
    },
    /// A provider should attempt fulfillment.
    NeedsProvider {
        /// Selected provider id.
        provider_id: String,
        /// Arguments for the selected provider.
        args: FulfillmentProviderArgs,
        /// Matching providers after fallback exclusions.
        ranked_providers: Vec<RankedFulfillmentProvider>,
        /// Human-readable summary for persistence.
        summary: String,
    },
    /// A user must change configuration or choose another path.
    NeedsUserInput {
        /// Typed reason planning cannot proceed automatically.
        reason: FulfillmentUserInputReason,
        /// Human-readable summary for persistence.
        summary: String,
    },
}

impl ComputedFulfillmentPlan {
    /// Top-level persisted decision for this computed plan.
    pub const fn decision(&self) -> FulfillmentPlanDecision {
        match self {
            Self::AlreadySatisfied { .. } => FulfillmentPlanDecision::AlreadySatisfied,
            Self::NeedsProvider { .. } => FulfillmentPlanDecision::NeedsProvider,
            Self::NeedsUserInput { .. } => FulfillmentPlanDecision::NeedsUserInput,
        }
    }

    /// Human-readable summary for persistence.
    pub fn summary(&self) -> &str {
        match self {
            Self::AlreadySatisfied { summary, .. }
            | Self::NeedsProvider { summary, .. }
            | Self::NeedsUserInput { summary, .. } => summary,
        }
    }

    /// Provider-selection compatible projection for existing request APIs.
    pub fn provider_selection_plan(&self) -> ProviderSelectionPlan {
        match self {
            Self::AlreadySatisfied { summary, .. } => ProviderSelectionPlan {
                decision: FulfillmentPlanDecision::AlreadySatisfied,
                selected_provider_id: None,
                ranked_providers: Vec::new(),
                summary: summary.clone(),
            },
            Self::NeedsProvider {
                provider_id,
                ranked_providers,
                summary,
                ..
            } => ProviderSelectionPlan {
                decision: FulfillmentPlanDecision::NeedsProvider,
                selected_provider_id: Some(provider_id.clone()),
                ranked_providers: ranked_providers.clone(),
                summary: summary.clone(),
            },
            Self::NeedsUserInput { summary, .. } => ProviderSelectionPlan {
                decision: FulfillmentPlanDecision::NeedsUserInput,
                selected_provider_id: None,
                ranked_providers: Vec::new(),
                summary: summary.clone(),
            },
        }
    }
}

/// Compute a deterministic fulfillment plan from request, library, and provider inputs.
pub fn compute_fulfillment_plan(
    input: FulfillmentPlanningInput<'_>,
) -> Result<ComputedFulfillmentPlan> {
    let request_id = input.request.id;
    let canonical_identity_id = input
        .request
        .target
        .canonical_identity_id
        .ok_or(Error::FulfillmentPlanningRequiresIdentity { request_id })?;

    if input.library.contains_resolved_identity {
        return Ok(ComputedFulfillmentPlan::AlreadySatisfied {
            canonical_identity_id,
            summary: format!("library already contains {canonical_identity_id}"),
        });
    }

    let selection = select_fulfillment_provider(
        ProviderSelectionContext::new(canonical_identity_id)
            .with_rejected_provider_ids(input.rejected_provider_ids),
        input.providers,
    )?;

    if let Some(provider_id) = selection.selected_provider_id {
        return Ok(ComputedFulfillmentPlan::NeedsProvider {
            provider_id,
            args: FulfillmentProviderArgs::new(canonical_identity_id),
            ranked_providers: selection.ranked_providers,
            summary: selection.summary,
        });
    }

    let reason = if input.rejected_provider_ids.is_empty() {
        FulfillmentUserInputReason::NoMatchingProvider
    } else {
        FulfillmentUserInputReason::NoRemainingProvider
    };

    Ok(ComputedFulfillmentPlan::NeedsUserInput {
        reason,
        summary: selection.summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{FulfillmentProviderCapabilities, FulfillmentProviderCapability};
    use kino_core::{
        CanonicalIdentityKind, Id, RequestRequester, RequestState, RequestTarget, TmdbId,
    };

    const MOVIE_PROVIDER: &[FulfillmentProviderCapability] =
        &[FulfillmentProviderCapability::MediaKind(
            CanonicalIdentityKind::Movie,
        )];
    const TV_PROVIDER: &[FulfillmentProviderCapability] =
        &[FulfillmentProviderCapability::MediaKind(
            CanonicalIdentityKind::TvSeries,
        )];
    const MOVIE_CAPS: FulfillmentProviderCapabilities<'static> =
        FulfillmentProviderCapabilities::new(MOVIE_PROVIDER);
    const TV_CAPS: FulfillmentProviderCapabilities<'static> =
        FulfillmentProviderCapabilities::new(TV_PROVIDER);

    #[test]
    fn already_satisfied_bypasses_provider_validation() {
        let request = request(Some(identity(550)));
        let providers = [
            ConfiguredFulfillmentProvider::new("duplicate", 0, MOVIE_CAPS),
            ConfiguredFulfillmentProvider::new(" duplicate ", 1, MOVIE_CAPS),
        ];

        let plan = compute_fulfillment_plan(FulfillmentPlanningInput::new(
            &request,
            FulfillmentLibraryState::new(true),
            &providers,
        ))
        .expect("already satisfied should not validate providers");

        assert_eq!(
            plan,
            ComputedFulfillmentPlan::AlreadySatisfied {
                canonical_identity_id: identity(550),
                summary: String::from("library already contains tmdb:movie:550"),
            }
        );
        assert_eq!(plan.decision(), FulfillmentPlanDecision::AlreadySatisfied);
    }

    #[test]
    fn matching_provider_returns_provider_plan_with_args() {
        let request = request(Some(identity(550)));
        let providers = [
            ConfiguredFulfillmentProvider::new("tv-provider", 100, TV_CAPS),
            ConfiguredFulfillmentProvider::new("movie-provider", 0, MOVIE_CAPS),
        ];

        let plan = compute_fulfillment_plan(FulfillmentPlanningInput::new(
            &request,
            FulfillmentLibraryState::new(false),
            &providers,
        ))
        .expect("provider should be selected");

        let ComputedFulfillmentPlan::NeedsProvider {
            provider_id,
            args,
            ranked_providers,
            summary,
        } = plan
        else {
            panic!("expected provider plan");
        };

        assert_eq!(provider_id, "movie-provider");
        assert_eq!(args, FulfillmentProviderArgs::new(identity(550)));
        assert_eq!(ranked_providers.len(), 1);
        assert_eq!(ranked_providers[0].provider_id, "movie-provider");
        assert_eq!(
            summary,
            "selected provider movie-provider for tmdb:movie:550"
        );
    }

    #[test]
    fn no_matching_provider_needs_user_input() {
        let request = request(Some(identity(550)));
        let providers = [ConfiguredFulfillmentProvider::new(
            "tv-provider",
            100,
            TV_CAPS,
        )];

        let plan = compute_fulfillment_plan(FulfillmentPlanningInput::new(
            &request,
            FulfillmentLibraryState::new(false),
            &providers,
        ))
        .expect("planning should succeed");

        assert_eq!(
            plan,
            ComputedFulfillmentPlan::NeedsUserInput {
                reason: FulfillmentUserInputReason::NoMatchingProvider,
                summary: String::from("no configured provider can satisfy tmdb:movie:550"),
            }
        );
        assert_eq!(plan.decision(), FulfillmentPlanDecision::NeedsUserInput);
    }

    #[test]
    fn rejected_matching_providers_need_user_input() {
        let request = request(Some(identity(550)));
        let providers = [ConfiguredFulfillmentProvider::new(
            "movie-provider",
            0,
            MOVIE_CAPS,
        )];
        let rejected = ["movie-provider"];

        let plan = compute_fulfillment_plan(
            FulfillmentPlanningInput::new(
                &request,
                FulfillmentLibraryState::new(false),
                &providers,
            )
            .with_rejected_provider_ids(&rejected),
        )
        .expect("planning should succeed");

        assert_eq!(
            plan,
            ComputedFulfillmentPlan::NeedsUserInput {
                reason: FulfillmentUserInputReason::NoRemainingProvider,
                summary: String::from(
                    "no remaining configured provider can satisfy tmdb:movie:550"
                ),
            }
        );
    }

    #[test]
    fn missing_canonical_identity_is_rejected() {
        let request = request(None);

        let err = compute_fulfillment_plan(FulfillmentPlanningInput::new(
            &request,
            FulfillmentLibraryState::new(false),
            &[],
        ))
        .expect_err("unresolved request should fail");

        assert!(matches!(
            err,
            Error::FulfillmentPlanningRequiresIdentity { request_id } if request_id == request.id
        ));
    }

    fn request(canonical_identity_id: Option<CanonicalIdentityId>) -> Request {
        let now = kino_core::Timestamp::now();
        Request {
            id: Id::new(),
            requester: RequestRequester::Anonymous,
            target: RequestTarget {
                raw_query: String::from("Inception (2010)"),
                canonical_identity_id,
            },
            state: RequestState::Planning,
            created_at: now,
            updated_at: now,
            plan_id: None,
            failure_reason: None,
        }
    }

    fn identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id must be positive"),
        }
    }
}
