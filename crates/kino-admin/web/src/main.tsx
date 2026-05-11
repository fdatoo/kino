import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { createBrowserRouter, RouterProvider } from 'react-router-dom';
import './styles.css';

const router = createBrowserRouter(
    [
        {
            path: '/',
            element: (
                <main className="app-shell">
                    <section className="panel">
                        <p className="eyebrow">Kino Admin</p>
                        <h1>Library operations</h1>
                        <p>
                            This workspace will host the browser-based tools for
                            managing Kino.
                        </p>
                    </section>
                </main>
            ),
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
