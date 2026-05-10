//! Movie request parsing and TMDB movie resolution.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    REQUEST_AUTO_RESOLVE_MIN_MARGIN, REQUEST_AUTO_RESOLVE_MIN_SCORE, REQUEST_MATCH_CANDIDATE_LIMIT,
};

/// Positive TMDB movie id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TmdbMovieId(u32);

impl TmdbMovieId {
    /// Construct a TMDB movie id when `value` is positive.
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Return the numeric TMDB id.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Parsed free-text movie request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovieRequest {
    /// Movie title to send to TMDB movie search.
    pub title: String,
    /// Optional release year parsed from the request.
    pub release_year: Option<i32>,
}

/// TMDB movie search result fields used by the resolver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TmdbMovieSearchResult {
    /// TMDB movie id.
    pub movie_id: TmdbMovieId,
    /// TMDB display title.
    pub title: String,
    /// Release year derived from TMDB `release_date`, when present.
    pub release_year: Option<i32>,
    /// TMDB popularity value, used only as a tiebreaker.
    pub popularity: f64,
}

impl TmdbMovieSearchResult {
    /// Construct a movie search result from the TMDB fields used by the resolver.
    pub fn new(
        movie_id: TmdbMovieId,
        title: impl Into<String>,
        release_year: Option<i32>,
        popularity: f64,
    ) -> Self {
        Self {
            movie_id,
            title: title.into(),
            release_year,
            popularity,
        }
    }

    /// Construct a result from TMDB's `release_date` field.
    pub fn from_release_date(
        movie_id: TmdbMovieId,
        title: impl Into<String>,
        release_date: Option<&str>,
        popularity: f64,
    ) -> Self {
        Self::new(
            movie_id,
            title,
            release_date.and_then(release_year_from_date),
            popularity,
        )
    }
}

/// Ranked TMDB movie candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovieCandidate {
    /// Request-local rank starting at one.
    pub rank: u32,
    /// TMDB movie id.
    pub movie_id: TmdbMovieId,
    /// TMDB display title.
    pub title: String,
    /// Release year when known.
    pub release_year: Option<i32>,
    /// TMDB popularity value.
    pub popularity: f64,
    /// Confidence score in the inclusive range `0.0..=1.0`.
    pub score: f64,
}

/// Resolved movie request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovieResolution {
    /// TMDB movie id selected by the resolver.
    pub movie_id: TmdbMovieId,
    /// TMDB display title selected by the resolver.
    pub title: String,
    /// Release year for the selected movie, when known.
    pub release_year: Option<i32>,
    /// Confidence score for the selected movie.
    pub score: f64,
}

/// Errors produced by movie request resolution.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum MovieResolveError {
    /// The request does not contain a usable movie title.
    #[error("movie request title is empty")]
    EmptyTitle,

    /// TMDB search returned no candidates.
    #[error("movie request has no TMDB candidates")]
    NoMovieCandidates,

    /// A candidate cannot be scored.
    #[error("tmdb movie candidate {movie_id} is invalid: {reason}")]
    InvalidMovieCandidate {
        /// Candidate TMDB movie id.
        movie_id: TmdbMovieId,
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// The resolver cannot select one candidate confidently.
    #[error("movie request did not have a confident TMDB match")]
    NoConfidentMovieMatch {
        /// Ranked candidates to present for disambiguation.
        candidates: Vec<MovieCandidate>,
    },
}

/// Result alias for movie request resolution.
pub type MovieResolveResult<T> = std::result::Result<T, MovieResolveError>;

/// TMDB movie resolver configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovieResolver {
    min_score: f64,
    min_margin: f64,
}

impl MovieResolver {
    /// Construct a resolver using request match confidence thresholds.
    pub const fn new() -> Self {
        Self {
            min_score: REQUEST_AUTO_RESOLVE_MIN_SCORE,
            min_margin: REQUEST_AUTO_RESOLVE_MIN_MARGIN,
        }
    }

    /// Construct a resolver with explicit confidence thresholds.
    pub const fn with_thresholds(min_score: f64, min_margin: f64) -> Self {
        Self {
            min_score,
            min_margin,
        }
    }

    /// Resolve a free-text request using TMDB movie search candidates.
    pub fn resolve(
        self,
        raw_query: &str,
        candidates: Vec<TmdbMovieSearchResult>,
    ) -> MovieResolveResult<MovieResolution> {
        let request = parse_movie_request(raw_query)?;
        self.resolve_parsed(request, candidates)
    }

    /// Resolve an already parsed request using TMDB movie search candidates.
    pub fn resolve_parsed(
        self,
        request: MovieRequest,
        candidates: Vec<TmdbMovieSearchResult>,
    ) -> MovieResolveResult<MovieResolution> {
        validate_movie_candidates(&candidates)?;

        let scored = score_movie_candidates(&request, candidates);
        let top = scored.first().ok_or(MovieResolveError::NoMovieCandidates)?;
        let next_score = scored.get(1).map_or(0.0, |candidate| candidate.score);
        if top.score < self.min_score || top.score - next_score < self.min_margin {
            return Err(MovieResolveError::NoConfidentMovieMatch {
                candidates: scored
                    .into_iter()
                    .take(REQUEST_MATCH_CANDIDATE_LIMIT)
                    .collect(),
            });
        }

        Ok(MovieResolution {
            movie_id: top.movie_id,
            title: top.title.clone(),
            release_year: top.release_year,
            score: top.score,
        })
    }
}

impl Default for MovieResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TmdbMovieId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parse a free-text movie request into a TMDB search query and optional year.
pub fn parse_movie_request(raw_query: &str) -> MovieResolveResult<MovieRequest> {
    let release_year = extract_year(raw_query);
    let title = clean_title(raw_query, release_year);
    if title.is_empty() {
        return Err(MovieResolveError::EmptyTitle);
    }

    Ok(MovieRequest {
        title,
        release_year,
    })
}

/// Parse the year from a TMDB `release_date` value.
pub fn release_year_from_date(value: &str) -> Option<i32> {
    if value.len() < 4 || !value.as_bytes()[0..4].iter().all(u8::is_ascii_digit) {
        return None;
    }

    value[0..4]
        .parse::<i32>()
        .ok()
        .filter(|year| (1000..=9999).contains(year))
}

fn validate_movie_candidates(candidates: &[TmdbMovieSearchResult]) -> MovieResolveResult<()> {
    if candidates.is_empty() {
        return Err(MovieResolveError::NoMovieCandidates);
    }

    for candidate in candidates {
        if candidate.title.trim().is_empty() {
            return Err(MovieResolveError::InvalidMovieCandidate {
                movie_id: candidate.movie_id,
                reason: "title is empty",
            });
        }
        if candidate
            .release_year
            .is_some_and(|year| !(1000..=9999).contains(&year))
        {
            return Err(MovieResolveError::InvalidMovieCandidate {
                movie_id: candidate.movie_id,
                reason: "release year is invalid",
            });
        }
        if !candidate.popularity.is_finite() || candidate.popularity < 0.0 {
            return Err(MovieResolveError::InvalidMovieCandidate {
                movie_id: candidate.movie_id,
                reason: "popularity must be finite and non-negative",
            });
        }
    }

    Ok(())
}

fn score_movie_candidates(
    request: &MovieRequest,
    candidates: Vec<TmdbMovieSearchResult>,
) -> Vec<MovieCandidate> {
    let max_popularity = candidates
        .iter()
        .map(|candidate| candidate.popularity)
        .fold(0.0, f64::max);

    let mut scored = candidates
        .into_iter()
        .map(|candidate| {
            let title_score = title_similarity(&request.title, &candidate.title);
            let year_score = year_match_score(request.release_year, candidate.release_year);
            let popularity_score = if max_popularity > 0.0 {
                candidate.popularity / max_popularity
            } else {
                0.0
            };
            let score =
                (title_score * 0.80 + year_score * 0.15 + popularity_score * 0.05).clamp(0.0, 1.0);

            MovieCandidate {
                rank: 0,
                movie_id: candidate.movie_id,
                title: candidate.title,
                release_year: candidate.release_year,
                popularity: candidate.popularity,
                score,
            }
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.popularity.total_cmp(&left.popularity))
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.movie_id.cmp(&right.movie_id))
    });

    for (index, candidate) in scored.iter_mut().enumerate() {
        if index >= u32::MAX as usize {
            candidate.rank = u32::MAX;
        } else {
            candidate.rank = index as u32 + 1;
        }
    }

    scored
}

fn clean_title(value: &str, year: Option<i32>) -> String {
    let mut title = value.to_owned();
    if let Some(year) = year {
        title = title.replace(&format!("({year})"), " ");
        title = title.replace(&year.to_string(), " ");
    }

    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(title_separator)
        .trim()
        .to_owned()
}

fn title_separator(character: char) -> bool {
    character.is_ascii_whitespace() || matches!(character, '-' | '_' | '.' | ':' | '/' | '\\')
}

fn extract_year(value: &str) -> Option<i32> {
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let end = index;
        if end - start != 4 || !year_position_looks_intentional(value, start, end) {
            continue;
        }

        if let Ok(year) = value[start..end].parse::<i32>()
            && (1888..=2100).contains(&year)
        {
            return Some(year);
        }
    }

    None
}

fn year_position_looks_intentional(value: &str, start: usize, end: usize) -> bool {
    let previous = value[..start].chars().next_back();
    let next = value[end..].chars().next();
    let standalone = previous.is_none() && next.is_none();
    let parenthesized = previous == Some('(') && next == Some(')');
    let trailing = next.is_none()
        && previous.is_some_and(|character| {
            character.is_ascii_whitespace() || matches!(character, '-' | '/' | ':')
        });

    standalone || parenthesized || trailing
}

fn year_match_score(query_year: Option<i32>, candidate_year: Option<i32>) -> f64 {
    match (query_year, candidate_year) {
        (Some(query), Some(candidate)) if query == candidate => 1.0,
        (Some(query), Some(candidate)) if (query - candidate).abs() == 1 => 0.65,
        (Some(_), Some(_)) => 0.0,
        (Some(_), None) => 0.35,
        (None, _) => 0.5,
    }
}

fn title_similarity(left: &str, right: &str) -> f64 {
    let left_tokens = normalized_tokens(left);
    let right_tokens = normalized_tokens(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    if left_tokens == right_tokens {
        return 1.0;
    }

    let mut unmatched = right_tokens.clone();
    let mut matches = 0usize;
    for token in &left_tokens {
        if let Some(index) = unmatched.iter().position(|candidate| candidate == token) {
            unmatched.swap_remove(index);
            matches += 1;
        }
    }

    (2.0 * matches as f64) / (left_tokens.len() + right_tokens.len()) as f64
}

fn normalized_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie_id(value: u32) -> TmdbMovieId {
        match TmdbMovieId::new(value) {
            Some(id) => id,
            None => panic!("test TMDB movie id must be positive"),
        }
    }

    fn candidate(
        id: u32,
        title: &str,
        release_year: Option<i32>,
        popularity: f64,
    ) -> TmdbMovieSearchResult {
        TmdbMovieSearchResult::new(movie_id(id), title, release_year, popularity)
    }

    #[test]
    fn parses_title_only_request() -> MovieResolveResult<()> {
        let request = parse_movie_request("The Matrix")?;

        assert_eq!(request.title, "The Matrix");
        assert_eq!(request.release_year, None);

        Ok(())
    }

    #[test]
    fn parses_trailing_year_request() -> MovieResolveResult<()> {
        let request = parse_movie_request("Inception 2010")?;

        assert_eq!(request.title, "Inception");
        assert_eq!(request.release_year, Some(2010));

        Ok(())
    }

    #[test]
    fn parses_parenthesized_year_request() -> MovieResolveResult<()> {
        let request = parse_movie_request("Inception (2010)")?;

        assert_eq!(request.title, "Inception");
        assert_eq!(request.release_year, Some(2010));

        Ok(())
    }

    #[test]
    fn keeps_title_years_that_are_not_release_year_locators() -> MovieResolveResult<()> {
        let request = parse_movie_request("2001 A Space Odyssey")?;

        assert_eq!(request.title, "2001 A Space Odyssey");
        assert_eq!(request.release_year, None);

        Ok(())
    }

    #[test]
    fn resolves_inception_2010_to_tmdb_movie() -> MovieResolveResult<()> {
        let resolution = MovieResolver::new().resolve(
            "Inception 2010",
            vec![
                candidate(27205, "Inception", Some(2010), 83.952),
                candidate(64956, "Inception: The Cobol Job", Some(2010), 12.0),
                candidate(443723, "Inception", Some(1917), 2.0),
            ],
        )?;

        assert_eq!(resolution.movie_id, movie_id(27205));
        assert_eq!(resolution.title, "Inception");
        assert_eq!(resolution.release_year, Some(2010));

        Ok(())
    }

    #[test]
    fn resolves_the_matrix_case_insensitively() -> MovieResolveResult<()> {
        for raw_query in ["The Matrix", "the matrix"] {
            let resolution = MovieResolver::new().resolve(
                raw_query,
                vec![
                    candidate(603, "The Matrix", Some(1999), 96.0),
                    candidate(604, "The Matrix Reloaded", Some(2003), 64.0),
                    candidate(605, "The Matrix Revolutions", Some(2003), 50.0),
                ],
            )?;

            assert_eq!(resolution.movie_id, movie_id(603));
            assert_eq!(resolution.title, "The Matrix");
        }

        Ok(())
    }

    #[test]
    fn ambiguous_matrix_returns_ranked_candidates() {
        let err = match MovieResolver::new().resolve(
            "Matrix",
            vec![
                candidate(603, "The Matrix", Some(1999), 96.0),
                candidate(604, "The Matrix Reloaded", Some(2003), 64.0),
                candidate(605, "The Matrix Revolutions", Some(2003), 50.0),
            ],
        ) {
            Ok(_) => panic!("ambiguous movie was resolved"),
            Err(err) => err,
        };

        let MovieResolveError::NoConfidentMovieMatch { candidates } = err else {
            panic!("unexpected error variant");
        };
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].rank, 1);
        assert_eq!(candidates[0].movie_id, movie_id(603));
        assert!(candidates[0].score > candidates[1].score);
        assert!(candidates[1].score > candidates[2].score);
    }

    #[test]
    fn rejects_empty_title_after_year_extraction() {
        let err = match parse_movie_request("2010") {
            Ok(_) => panic!("empty movie request was accepted"),
            Err(err) => err,
        };

        assert_eq!(err, MovieResolveError::EmptyTitle);
    }

    #[test]
    fn parses_tmdb_release_year() {
        assert_eq!(release_year_from_date("1999-03-31"), Some(1999));
        assert_eq!(release_year_from_date(""), None);
        assert_eq!(release_year_from_date("unknown"), None);
    }
}
