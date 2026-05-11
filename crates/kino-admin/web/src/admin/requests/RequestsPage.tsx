import { useCallback, useEffect, useState } from 'react';
import { Link, useSearchParams } from 'react-router-dom';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';
import { StatePill } from './StatePill';
import {
    allRequestStates,
    apiErrorMessage,
    formatState,
    formatTimestamp,
    isRequestState,
} from './request-utils';

type Request = components['schemas']['Request'];

export function RequestsPage() {
    const { clearToken } = useToken();
    const [searchParams, setSearchParams] = useSearchParams();
    const stateParam = searchParams.get('state');
    const stateFilter = isRequestState(stateParam) ? stateParam : 'all';
    const [requests, setRequests] = useState<Request[]>([]);
    const [error, setError] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);

    const loadRequests = useCallback(async () => {
        setIsLoading(true);
        setError(null);

        try {
            const {
                data,
                error: apiError,
                response,
            } = await apiClient.GET('/api/v1/requests', {
                params: {
                    query: {
                        limit: 50,
                        state: stateFilter === 'all' ? undefined : stateFilter,
                    },
                },
            });

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError(apiErrorMessage(apiError, 'Request list failed.'));
                return;
            }

            setRequests(data.requests);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Request list failed: ${err.message}`
                    : 'Request list failed.',
            );
        } finally {
            setIsLoading(false);
        }
    }, [clearToken, stateFilter]);

    useEffect(() => {
        void loadRequests();
    }, [loadRequests]);

    function handleStateFilter(nextState: string) {
        if (nextState === 'all') {
            setSearchParams({});
            return;
        }

        setSearchParams({ state: nextState });
    }

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Requests" />

            <section className="section-block" aria-labelledby="filters-title">
                <div className="section-heading">
                    <h2 id="filters-title">Filters</h2>
                    <button
                        disabled={isLoading}
                        type="button"
                        onClick={() => {
                            void loadRequests();
                        }}
                    >
                        Refresh
                    </button>
                </div>
                <label className="field request-filter">
                    <span>State</span>
                    <select
                        value={stateFilter}
                        onChange={(event) =>
                            handleStateFilter(event.currentTarget.value)
                        }
                    >
                        <option value="all">All states</option>
                        {allRequestStates().map((state) => (
                            <option key={state} value={state}>
                                {formatState(state)}
                            </option>
                        ))}
                    </select>
                </label>
            </section>

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}

            <section className="section-block" aria-labelledby="requests-title">
                <h2 id="requests-title">Existing requests</h2>
                {isLoading ? (
                    <p className="muted">Loading requests...</p>
                ) : (
                    <div className="table-wrap">
                        <table>
                            <thead>
                                <tr>
                                    <th>Target</th>
                                    <th>State</th>
                                    <th>Updated</th>
                                    <th aria-label="Actions" />
                                </tr>
                            </thead>
                            <tbody>
                                {requests.length === 0 ? (
                                    <tr>
                                        <td colSpan={4}>No requests found.</td>
                                    </tr>
                                ) : (
                                    requests.map((request) => (
                                        <tr key={request.id}>
                                            <td>{request.target.raw_query}</td>
                                            <td>
                                                <StatePill
                                                    state={request.state}
                                                />
                                            </td>
                                            <td>
                                                {formatTimestamp(
                                                    request.updated_at,
                                                )}
                                            </td>
                                            <td className="table-action">
                                                <Link
                                                    className="text-action"
                                                    to={`/requests/${request.id}`}
                                                >
                                                    Open
                                                </Link>
                                            </td>
                                        </tr>
                                    ))
                                )}
                            </tbody>
                        </table>
                    </div>
                )}
            </section>
        </main>
    );
}
