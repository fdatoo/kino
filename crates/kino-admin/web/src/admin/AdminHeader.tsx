import { FormEvent, useState } from 'react';
import { NavLink, useNavigate } from 'react-router-dom';

type AdminHeaderProps = {
    onSignOut: () => void;
    title: string;
};

export function AdminHeader({ onSignOut, title }: AdminHeaderProps) {
    const navigate = useNavigate();
    const [itemId, setItemId] = useState('');

    function handleOpenItem(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();

        const trimmedItemId = itemId.trim();
        if (trimmedItemId === '') {
            return;
        }

        navigate(`/items/${encodeURIComponent(trimmedItemId)}`);
        setItemId('');
    }

    return (
        <header className="top-bar">
            <div>
                <p className="eyebrow">Kino Admin</p>
                <h1>{title}</h1>
            </div>
            <div className="top-actions">
                <form
                    aria-label="Open catalog item"
                    className="item-jump-form"
                    onSubmit={handleOpenItem}
                >
                    <input
                        aria-label="Catalog item id"
                        onChange={(event) =>
                            setItemId(event.currentTarget.value)
                        }
                        placeholder="Catalog item id"
                        value={itemId}
                    />
                    <button type="submit">Open</button>
                </form>
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
