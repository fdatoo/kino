import { NavLink } from 'react-router-dom';

type AdminHeaderProps = {
    onSignOut: () => void;
    title: string;
};

export function AdminHeader({ onSignOut, title }: AdminHeaderProps) {
    return (
        <header className="top-bar">
            <div>
                <p className="eyebrow">Kino Admin</p>
                <h1>{title}</h1>
            </div>
            <div className="top-actions">
                <nav className="primary-nav" aria-label="Primary">
                    <NavLink to="/requests">Requests</NavLink>
                    <NavLink to="/tokens">Tokens</NavLink>
                    <NavLink to="/config">Config</NavLink>
                    <NavLink to="/sessions">Sessions</NavLink>
                </nav>
                <button type="button" onClick={onSignOut}>
                    Sign out
                </button>
            </div>
        </header>
    );
}
