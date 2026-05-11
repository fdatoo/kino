import { useCallback, useEffect, useState } from 'react';

import {
    TOKEN_CHANGED_EVENT,
    adminTokenStore,
    emitTokenChanged,
} from './token-store';

export function useToken() {
    const [token, setTokenState] = useState(() => adminTokenStore.get());

    useEffect(() => {
        const syncToken = () => {
            setTokenState(adminTokenStore.get());
        };

        window.addEventListener(TOKEN_CHANGED_EVENT, syncToken);
        window.addEventListener('storage', syncToken);

        return () => {
            window.removeEventListener(TOKEN_CHANGED_EVENT, syncToken);
            window.removeEventListener('storage', syncToken);
        };
    }, []);

    const setToken = useCallback((nextToken: string) => {
        adminTokenStore.set(nextToken);
        setTokenState(nextToken);
        emitTokenChanged();
    }, []);

    const clearToken = useCallback(() => {
        adminTokenStore.clear();
        setTokenState(null);
        emitTokenChanged();
    }, []);

    return { token, setToken, clearToken };
}
