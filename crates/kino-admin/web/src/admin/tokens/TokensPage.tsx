import { FormEvent, useCallback, useEffect, useState } from 'react';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';

type TokenSummary = components['schemas']['TokenSummary'];
type MintedToken = components['schemas']['CreateTokenResponse'];

export function TokensPage() {
    const { clearToken } = useToken();
    const [tokens, setTokens] = useState<TokenSummary[]>([]);
    const [label, setLabel] = useState('');
    const [mintedToken, setMintedToken] = useState<MintedToken | null>(null);
    const [status, setStatus] = useState<string | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);
    const [isMinting, setIsMinting] = useState(false);
    const [revokingTokenId, setRevokingTokenId] = useState<string | null>(null);

    const loadTokens = useCallback(async () => {
        setIsLoading(true);
        setError(null);

        try {
            const { data, response } = await apiClient.GET(
                '/api/v1/admin/tokens',
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError('Token list failed.');
                return;
            }

            setTokens(data.tokens);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Token list failed: ${err.message}`
                    : 'Token list failed.',
            );
        } finally {
            setIsLoading(false);
        }
    }, [clearToken]);

    useEffect(() => {
        void loadTokens();
    }, [loadTokens]);

    async function handleMint(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        const trimmedLabel = label.trim();

        if (trimmedLabel === '') {
            setError('Enter a token label.');
            return;
        }

        setIsMinting(true);
        setError(null);
        setStatus(null);
        setMintedToken(null);

        try {
            const { data, response } = await apiClient.POST(
                '/api/v1/admin/tokens',
                {
                    body: { label: trimmedLabel },
                },
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError('Token mint failed.');
                return;
            }

            setMintedToken(data);
            setLabel('');
            await loadTokens();
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Token mint failed: ${err.message}`
                    : 'Token mint failed.',
            );
        } finally {
            setIsMinting(false);
        }
    }

    async function handleRevoke(tokenId: string) {
        setRevokingTokenId(tokenId);
        setError(null);
        setStatus(null);

        try {
            const { response } = await apiClient.DELETE(
                '/api/v1/admin/tokens/{token_id}',
                {
                    params: { path: { token_id: tokenId } },
                },
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (response.status !== 204) {
                setError('Token revoke failed.');
                return;
            }

            setStatus('Token revoked.');
            await loadTokens();
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Token revoke failed: ${err.message}`
                    : 'Token revoke failed.',
            );
        } finally {
            setRevokingTokenId(null);
        }
    }

    async function handleCopyMintedToken() {
        if (mintedToken === null) {
            return;
        }

        if (navigator.clipboard === undefined) {
            setError('Clipboard is unavailable. Copy the token manually.');
            return;
        }

        try {
            await navigator.clipboard.writeText(mintedToken.token);
            setMintedToken(null);
            setStatus('Token copied.');
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Copy failed: ${err.message}`
                    : 'Copy failed.',
            );
        }
    }

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Device tokens" />

            <section className="section-block" aria-labelledby="mint-title">
                <h2 id="mint-title">Mint new token</h2>
                <form
                    className="inline-form"
                    onSubmit={(event) => {
                        void handleMint(event);
                    }}
                >
                    <label className="field inline-field">
                        <span>Label</span>
                        <input
                            name="label"
                            onChange={(event) =>
                                setLabel(event.currentTarget.value)
                            }
                            placeholder="Living room Apple TV"
                            value={label}
                        />
                    </label>
                    <button disabled={isMinting} type="submit">
                        {isMinting ? 'Minting...' : 'Mint token'}
                    </button>
                </form>

                {mintedToken !== null ? (
                    <div className="token-result" role="status">
                        <div>
                            <strong>{mintedToken.label}</strong>
                            <code>{mintedToken.token}</code>
                        </div>
                        <button
                            type="button"
                            onClick={() => {
                                void handleCopyMintedToken();
                            }}
                        >
                            Copy
                        </button>
                    </div>
                ) : null}
            </section>

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}
            {status !== null ? <p className="status">{status}</p> : null}

            <section className="section-block" aria-labelledby="tokens-title">
                <h2 id="tokens-title">Existing tokens</h2>
                {isLoading ? (
                    <p className="muted">Loading tokens...</p>
                ) : (
                    <div className="table-wrap">
                        <table>
                            <thead>
                                <tr>
                                    <th>Label</th>
                                    <th>Last seen</th>
                                    <th>Revoked</th>
                                    <th aria-label="Actions" />
                                </tr>
                            </thead>
                            <tbody>
                                {tokens.length === 0 ? (
                                    <tr>
                                        <td colSpan={4}>No tokens found.</td>
                                    </tr>
                                ) : (
                                    tokens.map((token) => (
                                        <tr key={token.token_id}>
                                            <td>{token.label}</td>
                                            <td>
                                                {formatTimestamp(
                                                    token.last_seen_at,
                                                )}
                                            </td>
                                            <td>
                                                {formatTimestamp(
                                                    token.revoked_at,
                                                )}
                                            </td>
                                            <td className="table-action">
                                                <button
                                                    disabled={
                                                        token.revoked_at !=
                                                            null ||
                                                        revokingTokenId ===
                                                            token.token_id
                                                    }
                                                    onClick={() =>
                                                        void handleRevoke(
                                                            token.token_id,
                                                        )
                                                    }
                                                    type="button"
                                                >
                                                    {revokingTokenId ===
                                                    token.token_id
                                                        ? 'Revoking...'
                                                        : 'Revoke'}
                                                </button>
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

function formatTimestamp(value: string | null | undefined): string {
    if (value === null || value === undefined) {
        return '--';
    }

    return new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
    }).format(new Date(value));
}
