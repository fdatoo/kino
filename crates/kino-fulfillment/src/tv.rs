//! TV request parsing and TMDB series resolution.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    REQUEST_AUTO_RESOLVE_MIN_MARGIN, REQUEST_AUTO_RESOLVE_MIN_SCORE, REQUEST_MATCH_CANDIDATE_LIMIT,
};

/// Positive TMDB TV series id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TmdbSeriesId(u32);

impl TmdbSeriesId {
    /// Construct a TMDB series id when `value` is positive.
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Return the numeric TMDB id.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Season coordinates in TMDB TV numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TvSeasonLocator {
    /// TMDB season number. Season zero represents specials.
    pub season_number: u32,
}

impl TvSeasonLocator {
    /// Construct a season locator.
    pub const fn new(season_number: u32) -> Self {
        Self { season_number }
    }
}

/// Episode coordinates in TMDB TV numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TvEpisodeLocator {
    /// TMDB season number. Season zero represents specials.
    pub season_number: u32,
    /// TMDB episode number within the season.
    pub episode_number: u32,
}

impl TvEpisodeLocator {
    /// Construct an episode locator when the episode number is positive.
    pub const fn new(season_number: u32, episode_number: u32) -> Option<Self> {
        if episode_number == 0 {
            None
        } else {
            Some(Self {
                season_number,
                episode_number,
            })
        }
    }
}

/// Parsed free-text TV request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TvRequest {
    /// Series title to send to TMDB TV search.
    pub title: String,
    /// Optional first-air year parsed from the request.
    pub first_air_year: Option<i32>,
    /// Optional requested season coordinates.
    pub season: Option<TvSeasonLocator>,
    /// Optional requested episode coordinates, present only for episode requests.
    pub episode: Option<TvEpisodeLocator>,
}

/// TMDB TV search result fields used by the resolver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TmdbTvSearchResult {
    /// TMDB series id.
    pub series_id: TmdbSeriesId,
    /// TMDB display name.
    pub name: String,
    /// First-air year derived from TMDB `first_air_date`, when present.
    pub first_air_year: Option<i32>,
    /// TMDB popularity value, used only as a tiebreaker.
    pub popularity: f64,
}

impl TmdbTvSearchResult {
    /// Construct a TV search result from the TMDB fields used by the resolver.
    pub fn new(
        series_id: TmdbSeriesId,
        name: impl Into<String>,
        first_air_year: Option<i32>,
        popularity: f64,
    ) -> Self {
        Self {
            series_id,
            name: name.into(),
            first_air_year,
            popularity,
        }
    }

    /// Construct a result from TMDB's `first_air_date` field.
    pub fn from_first_air_date(
        series_id: TmdbSeriesId,
        name: impl Into<String>,
        first_air_date: Option<&str>,
        popularity: f64,
    ) -> Self {
        Self::new(
            series_id,
            name,
            first_air_date.and_then(first_air_year_from_date),
            popularity,
        )
    }
}

/// Ranked TMDB series candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TvSeriesCandidate {
    /// Request-local rank starting at one.
    pub rank: u32,
    /// TMDB series id.
    pub series_id: TmdbSeriesId,
    /// TMDB display name.
    pub name: String,
    /// First-air year when known.
    pub first_air_year: Option<i32>,
    /// TMDB popularity value.
    pub popularity: f64,
    /// Confidence score in the inclusive range `0.0..=1.0`.
    pub score: f64,
}

/// Resolved TV request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TvResolution {
    /// TMDB series id selected by the resolver.
    pub series_id: TmdbSeriesId,
    /// TMDB display name selected by the resolver.
    pub series_name: String,
    /// First-air year for the selected series, when known.
    pub first_air_year: Option<i32>,
    /// Optional requested season coordinates.
    pub season: Option<TvSeasonLocator>,
    /// Optional requested episode coordinates, present only for episode requests.
    pub episode: Option<TvEpisodeLocator>,
    /// Confidence score for the selected series.
    pub score: f64,
}

/// Errors produced by TV request resolution.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum TvResolveError {
    /// The request does not contain a usable series title.
    #[error("tv request title is empty")]
    EmptyTitle,

    /// A request includes a TV locator that cannot be represented.
    #[error("tv locator {value} is invalid")]
    InvalidLocator {
        /// Raw locator text.
        value: String,
    },

    /// TMDB search returned no candidates.
    #[error("tv request has no TMDB series candidates")]
    NoSeriesCandidates,

    /// A candidate cannot be scored.
    #[error("tmdb tv candidate {series_id} is invalid: {reason}")]
    InvalidSeriesCandidate {
        /// Candidate TMDB series id.
        series_id: TmdbSeriesId,
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// The resolver cannot select one candidate confidently.
    #[error("tv request did not have a confident TMDB series match")]
    NoConfidentSeriesMatch {
        /// Ranked candidates to present for disambiguation.
        candidates: Vec<TvSeriesCandidate>,
    },
}

/// Result alias for TV request resolution.
pub type TvResolveResult<T> = std::result::Result<T, TvResolveError>;

/// TMDB TV resolver configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TvResolver {
    min_score: f64,
    min_margin: f64,
}

impl TvResolver {
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

    /// Resolve a free-text request using TMDB TV search candidates.
    pub fn resolve(
        self,
        raw_query: &str,
        candidates: Vec<TmdbTvSearchResult>,
    ) -> TvResolveResult<TvResolution> {
        let request = parse_tv_request(raw_query)?;
        self.resolve_parsed(request, candidates)
    }

    /// Resolve an already parsed request using TMDB TV search candidates.
    pub fn resolve_parsed(
        self,
        request: TvRequest,
        candidates: Vec<TmdbTvSearchResult>,
    ) -> TvResolveResult<TvResolution> {
        validate_series_candidates(&candidates)?;

        let scored = score_series_candidates(&request, candidates);
        let top = scored.first().ok_or(TvResolveError::NoSeriesCandidates)?;
        let next_score = scored.get(1).map_or(0.0, |candidate| candidate.score);
        if top.score < self.min_score || top.score - next_score < self.min_margin {
            return Err(TvResolveError::NoConfidentSeriesMatch {
                candidates: scored
                    .into_iter()
                    .take(REQUEST_MATCH_CANDIDATE_LIMIT)
                    .collect(),
            });
        }

        Ok(TvResolution {
            series_id: top.series_id,
            series_name: top.name.clone(),
            first_air_year: top.first_air_year,
            season: request.season,
            episode: request.episode,
            score: top.score,
        })
    }
}

impl Default for TvResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TmdbSeriesId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parse a free-text TV request into a TMDB search query and optional TV locator.
pub fn parse_tv_request(raw_query: &str) -> TvResolveResult<TvRequest> {
    let (without_locator, locator) = extract_tv_locator(raw_query)?;
    let first_air_year = extract_year(&without_locator);
    let title = clean_title(&without_locator, first_air_year);
    if title.is_empty() {
        return Err(TvResolveError::EmptyTitle);
    }
    let (season, episode) = match locator {
        Some(ParsedTvLocator::Season(season)) => (Some(season), None),
        Some(ParsedTvLocator::Episode(episode)) => (
            Some(TvSeasonLocator::new(episode.season_number)),
            Some(episode),
        ),
        None => (None, None),
    };

    Ok(TvRequest {
        title,
        first_air_year,
        season,
        episode,
    })
}

/// Parse the year from a TMDB `first_air_date` value.
pub fn first_air_year_from_date(value: &str) -> Option<i32> {
    if value.len() < 4 || !value.as_bytes()[0..4].iter().all(u8::is_ascii_digit) {
        return None;
    }

    value[0..4]
        .parse::<i32>()
        .ok()
        .filter(|year| (1000..=9999).contains(year))
}

fn validate_series_candidates(candidates: &[TmdbTvSearchResult]) -> TvResolveResult<()> {
    if candidates.is_empty() {
        return Err(TvResolveError::NoSeriesCandidates);
    }

    for candidate in candidates {
        if candidate.name.trim().is_empty() {
            return Err(TvResolveError::InvalidSeriesCandidate {
                series_id: candidate.series_id,
                reason: "name is empty",
            });
        }
        if candidate
            .first_air_year
            .is_some_and(|year| !(1000..=9999).contains(&year))
        {
            return Err(TvResolveError::InvalidSeriesCandidate {
                series_id: candidate.series_id,
                reason: "first-air year is invalid",
            });
        }
        if !candidate.popularity.is_finite() || candidate.popularity < 0.0 {
            return Err(TvResolveError::InvalidSeriesCandidate {
                series_id: candidate.series_id,
                reason: "popularity must be finite and non-negative",
            });
        }
    }

    Ok(())
}

fn score_series_candidates(
    request: &TvRequest,
    candidates: Vec<TmdbTvSearchResult>,
) -> Vec<TvSeriesCandidate> {
    let max_popularity = candidates
        .iter()
        .map(|candidate| candidate.popularity)
        .fold(0.0, f64::max);

    let mut scored = candidates
        .into_iter()
        .map(|candidate| {
            let title_score = title_similarity(&request.title, &candidate.name);
            let year_score = year_match_score(request.first_air_year, candidate.first_air_year);
            let popularity_score = if max_popularity > 0.0 {
                candidate.popularity / max_popularity
            } else {
                0.0
            };
            let score =
                (title_score * 0.80 + year_score * 0.15 + popularity_score * 0.05).clamp(0.0, 1.0);

            TvSeriesCandidate {
                rank: 0,
                series_id: candidate.series_id,
                name: candidate.name,
                first_air_year: candidate.first_air_year,
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
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.series_id.cmp(&right.series_id))
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

enum ParsedTvLocator {
    Season(TvSeasonLocator),
    Episode(TvEpisodeLocator),
}

fn extract_tv_locator(raw_query: &str) -> TvResolveResult<(String, Option<ParsedTvLocator>)> {
    let Some(match_range) =
        find_last_tv_locator(raw_query).or_else(|| find_last_named_season_locator(raw_query))
    else {
        return Ok((raw_query.to_owned(), None));
    };

    let raw_locator = &raw_query[match_range.clone()];
    let locator = parse_tv_locator(raw_locator)
        .or_else(|| parse_named_season_locator(raw_locator))
        .ok_or_else(|| TvResolveError::InvalidLocator {
            value: raw_locator.to_owned(),
        })?;
    let without_episode = format!(
        "{} {}",
        raw_query[..match_range.start].trim_end_matches(title_separator),
        raw_query[match_range.end..].trim_start_matches(title_separator)
    );

    Ok((without_episode, Some(locator)))
}

fn find_last_tv_locator(raw_query: &str) -> Option<std::ops::Range<usize>> {
    let bytes = raw_query.as_bytes();
    let mut index = 0usize;
    let mut found = None;
    while index < bytes.len() {
        if !bytes[index].eq_ignore_ascii_case(&b's') {
            index += 1;
            continue;
        }

        let Some((season_end, _season_number)) = read_digits(bytes, index + 1) else {
            index += 1;
            continue;
        };
        if season_end == index + 1 || season_end - index > 4 {
            index += 1;
            continue;
        }

        if season_end < bytes.len() && bytes[season_end].eq_ignore_ascii_case(&b'e') {
            let Some((episode_end, _episode_number)) = read_digits(bytes, season_end + 1) else {
                index += 1;
                continue;
            };
            if episode_end == season_end + 1 || episode_end - season_end > 4 {
                index += 1;
                continue;
            }
            if tv_locator_boundary(raw_query, index, episode_end) {
                found = Some(index..episode_end);
                index = episode_end;
            } else {
                index += 1;
            }
            continue;
        }

        if tv_locator_boundary(raw_query, index, season_end) {
            found = Some(index..season_end);
            index = season_end;
        } else {
            index += 1;
        }
    }

    found
}

fn find_last_named_season_locator(raw_query: &str) -> Option<std::ops::Range<usize>> {
    let bytes = raw_query.as_bytes();
    let mut index = 0usize;
    let mut found = None;
    while index < bytes.len() {
        if !ascii_starts_with_ignore_case(&bytes[index..], b"season") {
            index += 1;
            continue;
        }

        let season_end = index + b"season".len();
        if !word_boundary(raw_query, index, season_end) {
            index += 1;
            continue;
        }

        let digits_start = skip_season_separator(bytes, season_end);
        let Some((digits_end, _season_number)) = read_digits(bytes, digits_start) else {
            index += 1;
            continue;
        };
        if digits_end == digits_start || digits_end - digits_start > 4 {
            index += 1;
            continue;
        }
        if tv_locator_boundary(raw_query, index, digits_end) {
            found = Some(index..digits_end);
            index = digits_end;
        } else {
            index += 1;
        }
    }

    found
}

fn parse_tv_locator(raw_locator: &str) -> Option<ParsedTvLocator> {
    let bytes = raw_locator.as_bytes();
    if bytes
        .first()
        .is_none_or(|byte| !byte.eq_ignore_ascii_case(&b's'))
    {
        return None;
    }

    let (season_end, season_number) = read_digits(bytes, 1)?;
    if season_end == bytes.len() {
        return Some(ParsedTvLocator::Season(TvSeasonLocator::new(season_number)));
    }
    if !bytes[season_end].eq_ignore_ascii_case(&b'e') {
        return None;
    }

    let (episode_end, episode_number) = read_digits(bytes, season_end + 1)?;
    if episode_end != bytes.len() {
        return None;
    }

    TvEpisodeLocator::new(season_number, episode_number).map(ParsedTvLocator::Episode)
}

fn parse_named_season_locator(raw_locator: &str) -> Option<ParsedTvLocator> {
    let bytes = raw_locator.as_bytes();
    if !ascii_starts_with_ignore_case(bytes, b"season") {
        return None;
    }

    let digits_start = skip_season_separator(bytes, b"season".len());
    let (digits_end, season_number) = read_digits(bytes, digits_start)?;
    if digits_end != bytes.len() {
        return None;
    }

    Some(ParsedTvLocator::Season(TvSeasonLocator::new(season_number)))
}

fn ascii_starts_with_ignore_case(value: &[u8], prefix: &[u8]) -> bool {
    value.len() >= prefix.len()
        && value[..prefix.len()]
            .iter()
            .zip(prefix)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn skip_season_separator(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len()
        && (bytes[index].is_ascii_whitespace() || matches!(bytes[index], b':' | b'-' | b'_'))
    {
        index += 1;
    }

    index
}

fn read_digits(bytes: &[u8], mut index: usize) -> Option<(usize, u32)> {
    let start = index;
    let mut value = 0u32;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add(u32::from(bytes[index] - b'0'));
        index += 1;
    }

    (index > start).then_some((index, value))
}

fn tv_locator_boundary(value: &str, start: usize, end: usize) -> bool {
    let previous = value[..start].chars().next_back();
    let next = value[end..].chars().next();
    previous.is_none_or(|character| !character.is_ascii_alphanumeric())
        && next.is_none_or(|character| !character.is_ascii_alphanumeric())
}

fn word_boundary(value: &str, start: usize, end: usize) -> bool {
    let previous = value[..start].chars().next_back();
    let next = value[end..].chars().next();
    previous.is_none_or(|character| !character.is_ascii_alphanumeric())
        && next.is_none_or(|character| !character.is_ascii_alphabetic())
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
    let parenthesized = previous == Some('(') && next == Some(')');
    let trailing = next.is_none()
        && previous.is_some_and(|character| {
            character.is_ascii_whitespace() || matches!(character, '-' | '/' | ':')
        });

    parenthesized || trailing
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

    fn series_id(value: u32) -> TmdbSeriesId {
        match TmdbSeriesId::new(value) {
            Some(id) => id,
            None => panic!("test TMDB series id must be positive"),
        }
    }

    fn candidate(
        id: u32,
        name: &str,
        first_air_year: Option<i32>,
        popularity: f64,
    ) -> TmdbTvSearchResult {
        TmdbTvSearchResult::new(series_id(id), name, first_air_year, popularity)
    }

    #[test]
    fn parses_series_level_request() -> TvResolveResult<()> {
        let request = parse_tv_request("Breaking Bad")?;

        assert_eq!(request.title, "Breaking Bad");
        assert_eq!(request.first_air_year, None);
        assert_eq!(request.season, None);
        assert_eq!(request.episode, None);

        Ok(())
    }

    #[test]
    fn parses_season_level_request() -> TvResolveResult<()> {
        let request = parse_tv_request("Breaking Bad S01")?;

        assert_eq!(request.title, "Breaking Bad");
        assert_eq!(request.season, Some(TvSeasonLocator { season_number: 1 }));
        assert_eq!(request.episode, None);

        Ok(())
    }

    #[test]
    fn parses_named_season_level_request() -> TvResolveResult<()> {
        let request = parse_tv_request("Breaking Bad season 1")?;

        assert_eq!(request.title, "Breaking Bad");
        assert_eq!(request.season, Some(TvSeasonLocator { season_number: 1 }));
        assert_eq!(request.episode, None);

        Ok(())
    }

    #[test]
    fn parses_episode_level_request() -> TvResolveResult<()> {
        let request = parse_tv_request("Breaking Bad S01E03")?;

        assert_eq!(request.title, "Breaking Bad");
        assert_eq!(request.first_air_year, None);
        assert_eq!(request.season, Some(TvSeasonLocator { season_number: 1 }));
        assert_eq!(
            request.episode,
            Some(TvEpisodeLocator {
                season_number: 1,
                episode_number: 3
            })
        );

        Ok(())
    }

    #[test]
    fn parses_year_before_episode_locator() -> TvResolveResult<()> {
        let request = parse_tv_request("Breaking Bad (2008) - S01E03")?;

        assert_eq!(request.title, "Breaking Bad");
        assert_eq!(request.first_air_year, Some(2008));
        assert_eq!(request.season, Some(TvSeasonLocator { season_number: 1 }));
        assert_eq!(
            request.episode,
            Some(TvEpisodeLocator {
                season_number: 1,
                episode_number: 3
            })
        );

        Ok(())
    }

    #[test]
    fn rejects_episode_zero() {
        let err = match parse_tv_request("Breaking Bad S01E00") {
            Ok(_) => panic!("episode zero was accepted"),
            Err(err) => err,
        };

        assert_eq!(
            err,
            TvResolveError::InvalidLocator {
                value: "S01E00".to_owned()
            }
        );
    }

    #[test]
    fn resolves_series_request_to_tmdb_series() -> TvResolveResult<()> {
        let resolution = TvResolver::new().resolve(
            "Breaking Bad",
            vec![
                candidate(1396, "Breaking Bad", Some(2008), 683.584),
                candidate(30991, "Breaking In", Some(2011), 20.0),
            ],
        )?;

        assert_eq!(resolution.series_id, series_id(1396));
        assert_eq!(resolution.series_name, "Breaking Bad");
        assert_eq!(resolution.first_air_year, Some(2008));
        assert_eq!(resolution.season, None);
        assert_eq!(resolution.episode, None);

        Ok(())
    }

    #[test]
    fn resolves_season_request_to_tmdb_series_and_season() -> TvResolveResult<()> {
        let resolution = TvResolver::new().resolve(
            "Breaking Bad S01",
            vec![
                candidate(1396, "Breaking Bad", Some(2008), 683.584),
                candidate(30991, "Breaking In", Some(2011), 20.0),
            ],
        )?;

        assert_eq!(resolution.series_id, series_id(1396));
        assert_eq!(
            resolution.season,
            Some(TvSeasonLocator { season_number: 1 })
        );
        assert_eq!(resolution.episode, None);

        Ok(())
    }

    #[test]
    fn resolves_episode_request_to_tmdb_series_and_episode() -> TvResolveResult<()> {
        let resolution = TvResolver::new().resolve(
            "Breaking Bad S01E03",
            vec![
                candidate(1396, "Breaking Bad", Some(2008), 683.584),
                candidate(30991, "Breaking In", Some(2011), 20.0),
            ],
        )?;

        assert_eq!(resolution.series_id, series_id(1396));
        assert_eq!(
            resolution.season,
            Some(TvSeasonLocator { season_number: 1 })
        );
        assert_eq!(
            resolution.episode,
            Some(TvEpisodeLocator {
                season_number: 1,
                episode_number: 3
            })
        );

        Ok(())
    }

    #[test]
    fn ambiguous_series_returns_ranked_candidates() {
        let err = match TvResolver::new().resolve(
            "Dune",
            vec![
                candidate(1, "Dune", Some(2000), 20.0),
                candidate(2, "Dune", Some(2021), 19.5),
            ],
        ) {
            Ok(_) => panic!("ambiguous series was resolved"),
            Err(err) => err,
        };

        let TvResolveError::NoConfidentSeriesMatch { candidates } = err else {
            panic!("unexpected error variant");
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].rank, 1);
        assert_eq!(candidates[1].rank, 2);
    }

    #[test]
    fn parses_tmdb_first_air_year() {
        assert_eq!(first_air_year_from_date("2008-01-20"), Some(2008));
        assert_eq!(first_air_year_from_date(""), None);
        assert_eq!(first_air_year_from_date("unknown"), None);
    }
}
