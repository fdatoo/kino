import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link, useSearchParams } from 'react-router-dom';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';

const REFRESH_INTERVAL_MS = 5000;
const ACTIVE_SESSION_FILTER = 'active,idle';

type AdminPlaybackSession = components['schemas']['AdminPlaybackSession'];

export function SessionsPage() {
    const { clearToken } = useToken();
    const [searchParams] = useSearchParams();
    const selectedSessionId = searchParams.get('session');
    const [sessions, setSessions] = useState<AdminPlaybackSession[]>([]);
    const [error, setError] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);
    const [isRefreshing, setIsRefreshing] = useState(false);
    const [lastUpdatedAt, setLastUpdatedAt] = useState<Date | null>(null);

    const loadSessions = useCallback(
        async (showLoading: boolean) => {
            if (showLoading) {
                setIsLoading(true);
            } else {
                setIsRefreshing(true);
            }
            setError(null);

            try {
                const { data, response } = await apiClient.GET(
                    '/api/v1/admin/sessions',
                    {
                        params: {
                            query: { status: ACTIVE_SESSION_FILTER },
                        },
                    },
                );

                if (response.status === 401) {
                    clearToken();
                    return;
                }

                if (data === undefined) {
                    setError('Session list failed.');
                    return;
                }

                setSessions(data);
                setLastUpdatedAt(new Date());
            } catch (err) {
                setError(
                    err instanceof Error
                        ? `Session list failed: ${err.message}`
                        : 'Session list failed.',
                );
            } finally {
                setIsLoading(false);
                setIsRefreshing(false);
            }
        },
        [clearToken],
    );

    useEffect(() => {
        void loadSessions(true);
        const intervalId = window.setInterval(() => {
            void loadSessions(false);
        }, REFRESH_INTERVAL_MS);

        return () => {
            window.clearInterval(intervalId);
        };
    }, [loadSessions]);

    const selectedSession = useMemo(
        () =>
            sessions.find((session) => session.id === selectedSessionId) ??
            null,
        [selectedSessionId, sessions],
    );

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Active sessions" />

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}

            <section className="section-block" aria-labelledby="sessions-title">
                <div className="section-heading">
                    <h2 id="sessions-title">Current activity</h2>
                    <p className="muted compact-status" role="status">
                        {isRefreshing
                            ? 'Refreshing...'
                            : updatedLabel(lastUpdatedAt)}
                    </p>
                </div>

                {isLoading ? (
                    <p className="muted" role="status">
                        Loading sessions...
                    </p>
                ) : sessions.length === 0 ? (
                    <div className="empty-state">
                        <h3>No active sessions</h3>
                        <p className="muted">
                            No active or idle playback sessions are visible
                            right now.
                        </p>
                    </div>
                ) : (
                    <div className="sessions-layout">
                        <div className="table-wrap">
                            <table>
                                <thead>
                                    <tr>
                                        <th>User</th>
                                        <th>Item</th>
                                        <th>Variant</th>
                                        <th>Position</th>
                                        <th>Status</th>
                                        <th>Last seen</th>
                                        <th aria-label="Actions" />
                                    </tr>
                                </thead>
                                <tbody>
                                    {sessions.map((session) => (
                                        <tr
                                            className={
                                                session.id === selectedSessionId
                                                    ? 'selected-row'
                                                    : undefined
                                            }
                                            key={session.id}
                                        >
                                            <td>
                                                <code className="inline-code">
                                                    {session.user_id}
                                                </code>
                                            </td>
                                            <td>
                                                <code className="inline-code">
                                                    {session.media_item_id}
                                                </code>
                                            </td>
                                            <td>
                                                <code className="inline-code">
                                                    {session.variant_id}
                                                </code>
                                            </td>
                                            <td>
                                                {formatPosition(
                                                    session.position_seconds,
                                                )}
                                            </td>
                                            <td>
                                                <StatusPill
                                                    status={session.status}
                                                />
                                            </td>
                                            <td>
                                                {formatTimestamp(
                                                    session.last_seen_at,
                                                )}
                                            </td>
                                            <td className="table-action">
                                                <Link
                                                    className="button-link"
                                                    to={`/sessions?session=${encodeURIComponent(
                                                        session.id,
                                                    )}`}
                                                >
                                                    Details
                                                </Link>
                                            </td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>

                        <SessionDetail
                            selectedSessionId={selectedSessionId}
                            session={selectedSession}
                        />
                    </div>
                )}
            </section>
        </main>
    );
}

function SessionDetail({
    selectedSessionId,
    session,
}: {
    selectedSessionId: string | null;
    session: AdminPlaybackSession | null;
}) {
    if (selectedSessionId === null) {
        return null;
    }

    if (session === null) {
        return (
            <aside className="session-detail" aria-label="Session detail">
                <div className="detail-heading">
                    <h2>Session detail</h2>
                    <Link className="button-link secondary-link" to="/sessions">
                        Close
                    </Link>
                </div>
                <p className="muted">
                    Session {selectedSessionId} is not in the active view.
                </p>
            </aside>
        );
    }

    const recordJson = JSON.stringify(session, null, 2);

    return (
        <aside
            className="session-detail"
            aria-labelledby="session-detail-title"
        >
            <div className="detail-heading">
                <h2 id="session-detail-title">Session detail</h2>
                <Link className="button-link secondary-link" to="/sessions">
                    Close
                </Link>
            </div>

            <dl className="config-list">
                <DetailRow label="Session" value={session.id} />
                <DetailRow label="User" value={session.user_id} />
                <DetailRow label="Token" value={session.token_id} />
                <DetailRow label="Item" value={session.media_item_id} />
                <DetailRow label="Variant" value={session.variant_id} />
                <DetailRow label="Status" value={session.status} />
                <DetailRow
                    label="Position"
                    value={formatPosition(session.position_seconds)}
                />
                <DetailRow
                    label="Started"
                    value={formatTimestamp(session.started_at)}
                />
                <DetailRow
                    label="Last seen"
                    value={formatTimestamp(session.last_seen_at)}
                />
                <DetailRow
                    label="Ended"
                    value={formatTimestamp(session.ended_at)}
                />
            </dl>

            <section
                className="detail-section"
                aria-labelledby="timeline-title"
            >
                <h3 id="timeline-title">Status transitions</h3>
                <ol className="session-timeline">
                    <TimelineItem
                        label="active"
                        timestamp={session.started_at}
                    />
                    <TimelineItem
                        label={session.status}
                        timestamp={session.last_seen_at}
                    />
                    {session.ended_at !== null &&
                    session.ended_at !== undefined ? (
                        <TimelineItem
                            label="ended"
                            timestamp={session.ended_at}
                        />
                    ) : null}
                </ol>
            </section>

            <section className="detail-section" aria-labelledby="record-title">
                <h3 id="record-title">Record</h3>
                <pre className="record-json">{recordJson}</pre>
            </section>
        </aside>
    );
}

function DetailRow({ label, value }: { label: string; value: string }) {
    return (
        <div className="config-row">
            <dt>{label}</dt>
            <dd>
                <span>{value}</span>
            </dd>
        </div>
    );
}

function TimelineItem({
    label,
    timestamp,
}: {
    label: string;
    timestamp: string;
}) {
    return (
        <li>
            <StatusPill status={label} />
            <span>{formatTimestamp(timestamp)}</span>
        </li>
    );
}

function StatusPill({ status }: { status: string }) {
    return (
        <span className={`status-pill status-pill-${status}`}>{status}</span>
    );
}

function updatedLabel(value: Date | null): string {
    if (value === null) {
        return '';
    }

    return `Updated ${new Intl.DateTimeFormat(undefined, {
        timeStyle: 'medium',
    }).format(value)}`;
}

function formatTimestamp(value: string | null | undefined): string {
    if (value === null || value === undefined) {
        return '--';
    }

    return new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
    }).format(new Date(value));
}

function formatPosition(value: number | null | undefined): string {
    if (value === null || value === undefined) {
        return '--';
    }

    const totalSeconds = Math.max(0, Math.trunc(value));
    const seconds = totalSeconds % 60;
    const minutes = Math.floor(totalSeconds / 60) % 60;
    const hours = Math.floor(totalSeconds / 3600);

    if (hours > 0) {
        return `${hours}:${padTime(minutes)}:${padTime(seconds)}`;
    }

    return `${minutes}:${padTime(seconds)}`;
}

function padTime(value: number): string {
    return value.toString().padStart(2, '0');
}
