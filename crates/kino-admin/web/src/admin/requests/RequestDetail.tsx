import { FormEvent, useCallback, useEffect, useState } from 'react';
import { Link, useParams } from 'react-router-dom';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';
import { StatePill } from './StatePill';
import { apiErrorMessage, formatState, formatTimestamp } from './request-utils';

type RequestDetailResponse = components['schemas']['RequestDetail'];
type ManualImportResponse = components['schemas']['ManualImportResponse'];
type RequestMatchCandidate = components['schemas']['RequestMatchCandidate'];

export function RequestDetail() {
    const { id } = useParams<{ id: string }>();
    const { clearToken } = useToken();
    const [detail, setDetail] = useState<RequestDetailResponse | null>(null);
    const [path, setPath] = useState('');
    const [status, setStatus] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [importResult, setImportResult] =
        useState<ManualImportResponse | null>(null);
    const [resolveCandidates, setResolveCandidates] = useState<
        RequestMatchCandidate[]
    >([]);
    const [isLoading, setIsLoading] = useState(true);
    const [isSubmitting, setIsSubmitting] = useState(false);
    const [isResolving, setIsResolving] = useState(false);

    const loadRequest = useCallback(async () => {
        if (id === undefined) {
            setError('Request id is missing.');
            setIsLoading(false);
            return;
        }

        setIsLoading(true);
        setError(null);

        try {
            const {
                data,
                error: apiError,
                response,
            } = await apiClient.GET('/api/v1/requests/{id}', {
                params: { path: { id } },
            });

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError(apiErrorMessage(apiError, 'Request load failed.'));
                return;
            }

            setDetail(data);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Request load failed: ${err.message}`
                    : 'Request load failed.',
            );
        } finally {
            setIsLoading(false);
        }
    }, [clearToken, id]);

    useEffect(() => {
        void loadRequest();
    }, [loadRequest]);

    async function handleManualImport(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();

        if (id === undefined) {
            setError('Request id is missing.');
            return;
        }

        const trimmedPath = path.trim();
        if (trimmedPath === '') {
            setError('Enter a filesystem path.');
            return;
        }

        setIsSubmitting(true);
        setError(null);
        setStatus(null);
        setImportResult(null);

        try {
            const {
                data,
                error: apiError,
                response,
            } = await apiClient.POST(
                '/api/v1/admin/requests/{id}/manual-import',
                {
                    params: { path: { id } },
                    body: { path: trimmedPath },
                },
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError(apiErrorMessage(apiError, 'Manual import failed.'));
                return;
            }

            setDetail(data.request);
            setImportResult(data);
            setPath('');
            setStatus('Manual import started.');
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Manual import failed: ${err.message}`
                    : 'Manual import failed.',
            );
        } finally {
            setIsSubmitting(false);
        }
    }

    async function handleResolve() {
        if (id === undefined) {
            setError('Request id is missing.');
            return;
        }

        setIsResolving(true);
        setError(null);
        setStatus(null);
        setResolveCandidates([]);

        try {
            const {
                data,
                error: apiError,
                response,
            } = await apiClient.POST('/api/v1/requests/{id}/resolve', {
                params: { path: { id } },
            });

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError(apiErrorMessage(apiError, 'Resolve failed.'));
                return;
            }

            setResolveCandidates(data.candidates);
            setStatus('Resolve completed.');
            await loadRequest();
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Resolve failed: ${err.message}`
                    : 'Resolve failed.',
            );
        } finally {
            setIsResolving(false);
        }
    }

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Request detail" />

            <Link className="text-action" to="/requests">
                Back to requests
            </Link>

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}
            {status !== null ? <p className="status">{status}</p> : null}

            {isLoading ? <p className="muted">Loading request...</p> : null}

            {detail !== null ? (
                <>
                    <section
                        className="section-block"
                        aria-labelledby="request-title"
                    >
                        <div className="detail-heading">
                            <div>
                                <p className="eyebrow">Request</p>
                                <h2 id="request-title">
                                    {detail.request.target.raw_query}
                                </h2>
                            </div>
                            <StatePill state={detail.request.state} />
                        </div>

                        <dl className="config-list request-summary">
                            <SummaryRow
                                label="Request id"
                                value={detail.request.id}
                            />
                            <SummaryRow
                                label="Created"
                                value={formatTimestamp(
                                    detail.request.created_at,
                                )}
                            />
                            <SummaryRow
                                label="Updated"
                                value={formatTimestamp(
                                    detail.request.updated_at,
                                )}
                            />
                            <SummaryRow
                                label="Canonical identity"
                                value={
                                    detail.request.target
                                        .canonical_identity_id ?? '--'
                                }
                            />
                        </dl>
                    </section>

                    <section
                        className="section-block"
                        aria-labelledby="resolve-title"
                    >
                        <div className="section-heading">
                            <h2 id="resolve-title">Resolve</h2>
                            <button
                                disabled={isResolving}
                                onClick={() => {
                                    void handleResolve();
                                }}
                                type="button"
                            >
                                {isResolving ? 'Resolving...' : 'Resolve'}
                            </button>
                        </div>

                        {resolveCandidates.length > 0 ? (
                            <div className="table-wrap">
                                <table>
                                    <thead>
                                        <tr>
                                            <th>Rank</th>
                                            <th>Title</th>
                                            <th>Year</th>
                                            <th>Identity</th>
                                            <th>Score</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {resolveCandidates.map((candidate) => (
                                            <tr
                                                key={
                                                    candidate.canonical_identity_id
                                                }
                                            >
                                                <td>{candidate.rank}</td>
                                                <td>{candidate.title}</td>
                                                <td>
                                                    {candidate.year ?? '--'}
                                                </td>
                                                <td>
                                                    <code className="inline-code">
                                                        {
                                                            candidate.canonical_identity_id
                                                        }
                                                    </code>
                                                </td>
                                                <td>
                                                    {formatScore(
                                                        candidate.score,
                                                    )}
                                                </td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            </div>
                        ) : (
                            <p className="muted">No resolve candidates.</p>
                        )}
                    </section>

                    <section
                        className="section-block"
                        aria-labelledby="manual-import-title"
                    >
                        <h2 id="manual-import-title">Manual import</h2>
                        {detail.request.state === 'fulfilling' ? (
                            <form
                                className="inline-form"
                                onSubmit={(event) => {
                                    void handleManualImport(event);
                                }}
                            >
                                <label className="field inline-field">
                                    <span>Filesystem path</span>
                                    <input
                                        name="path"
                                        onChange={(event) =>
                                            setPath(event.currentTarget.value)
                                        }
                                        placeholder="/srv/imports/movie.mkv"
                                        value={path}
                                    />
                                </label>
                                <button disabled={isSubmitting} type="submit">
                                    {isSubmitting
                                        ? 'Starting...'
                                        : 'Start import'}
                                </button>
                            </form>
                        ) : (
                            <p className="muted">
                                Manual import is available from fulfilling
                                requests. Current state:{' '}
                                {formatState(detail.request.state)}.
                            </p>
                        )}

                        {importResult !== null ? (
                            <dl className="config-list import-result">
                                <SummaryRow
                                    label="Provider"
                                    value={importResult.provider_id}
                                />
                                <SummaryRow
                                    label="Job"
                                    value={importResult.job_id}
                                />
                                <SummaryRow
                                    label="Path"
                                    value={importResult.path}
                                />
                            </dl>
                        ) : null}
                    </section>

                    <section
                        className="section-block"
                        aria-labelledby="events-title"
                    >
                        <h2 id="events-title">Status events</h2>
                        <div className="table-wrap">
                            <table>
                                <thead>
                                    <tr>
                                        <th>Time</th>
                                        <th>Transition</th>
                                        <th>Message</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {detail.status_events.map((event) => (
                                        <tr key={event.id}>
                                            <td>
                                                {formatTimestamp(
                                                    event.occurred_at,
                                                )}
                                            </td>
                                            <td>
                                                {event.from_state === null ||
                                                event.from_state === undefined
                                                    ? formatState(
                                                          event.to_state,
                                                      )
                                                    : `${formatState(
                                                          event.from_state,
                                                      )} to ${formatState(
                                                          event.to_state,
                                                      )}`}
                                            </td>
                                            <td>{event.message ?? '--'}</td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>
                    </section>
                </>
            ) : null}
        </main>
    );
}

function formatScore(score: number) {
    return score.toFixed(3);
}

function SummaryRow({ label, value }: { label: string; value: string }) {
    return (
        <div className="config-row">
            <dt>{label}</dt>
            <dd>
                <span>{value}</span>
            </dd>
        </div>
    );
}
