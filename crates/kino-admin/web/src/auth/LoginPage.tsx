import { FormEvent, useState } from 'react';
import { Navigate, useLocation, useNavigate } from 'react-router-dom';

import { createApiClient } from '../api/client';
import { useToken } from './use-token';

type LocationState = {
    from?: {
        pathname?: string;
    };
};

export function LoginPage() {
    const navigate = useNavigate();
    const location = useLocation();
    const { token, setToken, clearToken } = useToken();
    const [candidateToken, setCandidateToken] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [isSubmitting, setIsSubmitting] = useState(false);

    if (token !== null) {
        return <Navigate to="/tokens" replace />;
    }

    const from = (location.state as LocationState | null)?.from?.pathname;
    const targetPath =
        from !== undefined && from !== '/login' ? from : '/tokens';

    async function handleSubmit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        const trimmedToken = candidateToken.trim();

        if (trimmedToken === '') {
            setError('Enter the bootstrap token.');
            return;
        }

        setError(null);
        setIsSubmitting(true);

        try {
            const validationClient = createApiClient({
                headers: { authorization: `Bearer ${trimmedToken}` },
            });
            const { response } = await validationClient.GET(
                '/api/v1/admin/tokens',
            );

            if (response.status === 200) {
                setToken(trimmedToken);
                navigate(targetPath, { replace: true });
                return;
            }

            if (response.status === 401) {
                clearToken();
                setError('Token was rejected.');
                return;
            }

            setError('Token validation failed.');
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Token validation failed: ${err.message}`
                    : 'Token validation failed.',
            );
        } finally {
            setIsSubmitting(false);
        }
    }

    return (
        <main className="auth-shell">
            <section className="auth-panel" aria-labelledby="login-title">
                <p className="eyebrow">Kino Admin</p>
                <h1 id="login-title">Enter admin token</h1>
                <form
                    className="form-stack"
                    onSubmit={(event) => {
                        void handleSubmit(event);
                    }}
                >
                    <label className="field">
                        <span>Bootstrap token</span>
                        <input
                            autoComplete="off"
                            autoFocus
                            name="token"
                            onChange={(event) =>
                                setCandidateToken(event.currentTarget.value)
                            }
                            type="password"
                            value={candidateToken}
                        />
                    </label>
                    {error !== null ? (
                        <p className="status status-error" role="alert">
                            {error}
                        </p>
                    ) : null}
                    <button disabled={isSubmitting} type="submit">
                        {isSubmitting ? 'Validating...' : 'Continue'}
                    </button>
                </form>
            </section>
        </main>
    );
}
