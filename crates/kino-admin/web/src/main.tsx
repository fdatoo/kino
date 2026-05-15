import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import {
    createBrowserRouter,
    Navigate,
    RouterProvider,
} from 'react-router-dom';
import { ConfigPage } from './admin/config/ConfigPage';
import { ItemDetailPage } from './admin/items/ItemDetailPage';
import { PairingsPage } from './admin/pairings/PairingsPage';
import { RequestDetail } from './admin/requests/RequestDetail';
import { RequestsPage } from './admin/requests/RequestsPage';
import { SessionsPage } from './admin/sessions/SessionsPage';
import { TokensPage } from './admin/tokens/TokensPage';
import { AuthGate } from './auth/AuthGate';
import { LoginPage } from './auth/LoginPage';
import './styles.css';

const router = createBrowserRouter(
    [
        {
            path: '/login',
            element: <LoginPage />,
        },
        {
            element: <AuthGate />,
            children: [
                {
                    path: '/',
                    element: <Navigate to="/tokens" replace />,
                },
                {
                    path: '/tokens',
                    element: <TokensPage />,
                },
                {
                    path: '/pairings',
                    element: <PairingsPage />,
                },
                {
                    path: '/requests',
                    element: <RequestsPage />,
                },
                {
                    path: '/requests/:id',
                    element: <RequestDetail />,
                },
                {
                    path: '/config',
                    element: <ConfigPage />,
                },
                {
                    path: '/items/:id',
                    element: <ItemDetailPage />,
                },
                {
                    path: '/sessions',
                    element: <SessionsPage />,
                },
            ],
        },
    ],
    { basename: '/admin' },
);

const rootElement = document.getElementById('root');

if (rootElement === null) {
    throw new Error('root element not found');
}

createRoot(rootElement).render(
    <StrictMode>
        <RouterProvider router={router} />
    </StrictMode>,
);
