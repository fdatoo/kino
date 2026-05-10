# Match Scoring And Disambiguation

Linear: F-237

## Goal

Rank resolver candidates for a pending request and decide whether resolution is
confident enough to continue automatically. When confidence is low, keep the
request active but park it in `needs_disambiguation` with the top candidates
available from the request detail API.

## Scoring

Candidate confidence is a weighted score:

- normalized title similarity is the dominant signal
- year agreement is a secondary signal when the request includes a year
- popularity is a low-weight tiebreaker within the returned candidate set

A candidate auto-resolves only when its score is above the confidence threshold
and it is sufficiently ahead of the next candidate. This avoids auto-selecting
between two plausible records with nearly identical titles.

## Persistence

`needs_disambiguation` is a durable request state. Low-confidence scoring
stores the ranked top candidates in `request_match_candidates`; high-confidence
scoring clears any parked candidates, writes the winning canonical identity id
onto the request projection, and transitions the request to `resolved`.

## API

The internal request API exposes candidate scoring as:

`POST /api/requests/:id/matches`

The response is the standard request detail projection. Parked requests include
their ranked candidates in that projection, so a later UI or CLI can present the
same server-side ordering.
