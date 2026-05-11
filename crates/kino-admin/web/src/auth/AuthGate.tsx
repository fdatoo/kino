import { Navigate, Outlet, useLocation } from 'react-router-dom';

import { useToken } from './use-token';

export function AuthGate() {
    const location = useLocation();
    const { token } = useToken();

    if (token === null) {
        return <Navigate to="/login" replace state={{ from: location }} />;
    }

    return <Outlet />;
}
