import { useCallback, useEffect, useState } from 'react';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';

const REFRESH_INTERVAL_MS = 5000;

type AdminPairingSummary = components['schemas']['AdminPairingSummary'];
type ApprovePairingResponse = components['schemas']['ApprovePairingResponse'];
type PairingAction = 'approve' | 'reject';

export function PairingsPage() {
    const { clearToken } = useToken();
    const [pairings, setPairings] = useState<AdminPairingSummary[]>([]);
    const [status, setStatus] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);
    const [isRefreshing, setIsRefreshing] = useState(false);
    const [lastUpdatedAt, setLastUpdatedAt] = useState<Date | null>(null);
    const [pairingActions, setPairingActions] = useState<
        Record<string, PairingAction>
    >({});

    const loadPairings = useCallback(
        async (showLoading: boolean) => {
            if (showLoading) {
                setIsLoading(true);
            } else {
                setIsRefreshing(true);
            }
            setError(null);

            try {
                const { data, response } = await apiClient.GET(
                    '/api/v1/admin/pairings',
                );

                if (response.status === 401) {
                    clearToken();
                    return;
                }

                if (data === undefined) {
                    setError('Pairing list failed.');
                    return;
                }

                setPairings(data.pairings);
                setLastUpdatedAt(new Date());
            } catch (err) {
                setError(
                    err instanceof Error
                        ? `Pairing list failed: ${err.message}`
                        : 'Pairing list failed.',
                );
            } finally {
                setIsLoading(false);
                setIsRefreshing(false);
            }
        },
        [clearToken],
    );

    useEffect(() => {
        void loadPairings(true);
        const intervalId = window.setInterval(() => {
            void loadPairings(false);
        }, REFRESH_INTERVAL_MS);

        return () => {
            window.clearInterval(intervalId);
        };
    }, [loadPairings]);

    async function handleApprove(pairing: AdminPairingSummary) {
        setPairingAction(pairing.pairing_id, 'approve');
        setError(null);
        setStatus(null);

        try {
            const { data, response } = await apiClient.POST(
                '/api/v1/admin/pairings/{pairing_id}/approve',
                {
                    params: {
                        path: { pairing_id: pairing.pairing_id },
                    },
                },
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError('Pairing approval failed.');
                return;
            }

            setStatus(approvedStatus(pairing, data));
            await loadPairings(false);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Pairing approval failed: ${err.message}`
                    : 'Pairing approval failed.',
            );
        } finally {
            clearPairingAction(pairing.pairing_id);
        }
    }

    async function handleReject(pairing: AdminPairingSummary) {
        setPairingAction(pairing.pairing_id, 'reject');
        setError(null);
        setStatus(null);

        try {
            const { response } = await apiClient.POST(
                '/api/v1/admin/pairings/{pairing_id}/reject',
                {
                    params: {
                        path: { pairing_id: pairing.pairing_id },
                    },
                },
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (response.status !== 204) {
                setError('Pairing rejection failed.');
                return;
            }

            setStatus('Pairing rejected.');
            await loadPairings(false);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Pairing rejection failed: ${err.message}`
                    : 'Pairing rejection failed.',
            );
        } finally {
            clearPairingAction(pairing.pairing_id);
        }
    }

    function setPairingAction(pairingId: string, action: PairingAction) {
        setPairingActions((current) => ({ ...current, [pairingId]: action }));
    }

    function clearPairingAction(pairingId: string) {
        setPairingActions((current) => {
            const next = { ...current };
            delete next[pairingId];
            return next;
        });
    }

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Pending pairings" />

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}
            {status !== null ? (
                <div className="status status-banner" role="status">
                    <span>{status}</span>
                    <button
                        className="secondary-button"
                        onClick={() => setStatus(null)}
                        type="button"
                    >
                        Dismiss
                    </button>
                </div>
            ) : null}

            <section className="section-block" aria-labelledby="pairings-title">
                <div className="section-heading">
                    <h2 id="pairings-title">Pairing requests</h2>
                    <p className="muted compact-status" role="status">
                        {isRefreshing
                            ? 'Refreshing...'
                            : updatedLabel(lastUpdatedAt)}
                    </p>
                </div>

                {isLoading ? (
                    <p className="muted" role="status">
                        Loading pairings...
                    </p>
                ) : (
                    <div className="table-wrap">
                        <table>
                            <thead>
                                <tr>
                                    <th>Device</th>
                                    <th>Platform</th>
                                    <th>Code</th>
                                    <th>Requested</th>
                                    <th>Expires in</th>
                                    <th aria-label="Actions" />
                                </tr>
                            </thead>
                            <tbody>
                                {pairings.length === 0 ? (
                                    <tr>
                                        <td colSpan={6}>
                                            No pending pairings.
                                        </td>
                                    </tr>
                                ) : (
                                    pairings.map((pairing) => {
                                        const action =
                                            pairingActions[
                                                pairing.pairing_id
                                            ] ?? null;

                                        return (
                                            <tr key={pairing.pairing_id}>
                                                <td>{pairing.device_name}</td>
                                                <td>{pairing.platform}</td>
                                                <td>
                                                    <code className="inline-code pairing-code">
                                                        {pairing.code}
                                                    </code>
                                                </td>
                                                <td>
                                                    {formatTimestamp(
                                                        pairing.created_at,
                                                    )}
                                                </td>
                                                <td>
                                                    {formatExpiresIn(
                                                        pairing.expires_at,
                                                    )}
                                                </td>
                                                <td className="table-action">
                                                    <div className="pairing-actions">
                                                        <button
                                                            disabled={
                                                                action !== null
                                                            }
                                                            onClick={() =>
                                                                void handleApprove(
                                                                    pairing,
                                                                )
                                                            }
                                                            type="button"
                                                        >
                                                            {action ===
                                                            'approve'
                                                                ? 'Approving...'
                                                                : 'Approve'}
                                                        </button>
                                                        <button
                                                            disabled={
                                                                action !== null
                                                            }
                                                            onClick={() =>
                                                                void handleReject(
                                                                    pairing,
                                                                )
                                                            }
                                                            type="button"
                                                        >
                                                            {action === 'reject'
                                                                ? 'Rejecting...'
                                                                : 'Reject'}
                                                        </button>
                                                    </div>
                                                </td>
                                            </tr>
                                        );
                                    })
                                )}
                            </tbody>
                        </table>
                    </div>
                )}
            </section>
        </main>
    );
}

function approvedStatus(
    pairing: AdminPairingSummary,
    response: ApprovePairingResponse,
): string {
    return `Token ending ...${response.token_preview} minted for ${pairing.device_name}.`;
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

function formatExpiresIn(value: string): string {
    const remainingSeconds = Math.max(
        0,
        Math.ceil((new Date(value).getTime() - Date.now()) / 1000),
    );

    if (remainingSeconds === 0) {
        return 'expired';
    }

    const seconds = remainingSeconds % 60;
    const minutes = Math.floor(remainingSeconds / 60) % 60;
    const hours = Math.floor(remainingSeconds / 3600);

    if (hours > 0) {
        return `${hours}h ${padTime(minutes)}m ${padTime(seconds)}s`;
    }

    if (minutes > 0) {
        return `${minutes}m ${padTime(seconds)}s`;
    }

    return `${seconds}s`;
}

function padTime(value: number): string {
    return value.toString().padStart(2, '0');
}
