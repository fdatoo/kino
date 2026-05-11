import { act, cleanup, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';

import type { components } from '../../api/schema';

const mockClient = vi.hoisted(() => ({
    DELETE: vi.fn(),
    GET: vi.fn(),
    POST: vi.fn(),
    use: vi.fn(),
}));

vi.mock('openapi-fetch', () => ({
    default: vi.fn(() => mockClient),
}));

import { SessionsPage } from './SessionsPage';

type AdminPlaybackSession = components['schemas']['AdminPlaybackSession'];
type SessionsResult = {
    data: AdminPlaybackSession[];
    response: Response;
};

const activeSession = {
    ended_at: null,
    id: '018f0000-0000-7000-8000-000000000011',
    last_seen_at: '2026-05-11T12:05:00Z',
    media_item_id: '018f0000-0000-7000-8000-000000000022',
    position_seconds: 125,
    started_at: '2026-05-11T12:00:00Z',
    status: 'active',
    token_id: '018f0000-0000-7000-8000-000000000033',
    user_id: '018f0000-0000-7000-8000-000000000044',
    variant_id: 'source-file:main',
} satisfies AdminPlaybackSession;

beforeEach(() => {
    mockClient.DELETE.mockReset();
    mockClient.GET.mockReset();
    mockClient.POST.mockReset();
    mockClient.use.mockReset();
});

afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.useRealTimers();
});

test('separates loading from an empty session list', async () => {
    let resolveSessions: ((value: SessionsResult) => void) | null = null;
    mockClient.GET.mockReturnValueOnce(
        new Promise<SessionsResult>((resolve) => {
            resolveSessions = resolve;
        }),
    );

    render(
        <MemoryRouter initialEntries={['/sessions']}>
            <SessionsPage />
        </MemoryRouter>,
    );

    expect(screen.getByText('Loading sessions...')).toBeTruthy();

    await act(async () => {
        if (resolveSessions === null) {
            throw new Error('session resolver not assigned');
        }

        resolveSessions({
            data: [],
            response: new Response(null, { status: 200 }),
        });
        await Promise.resolve();
    });

    expect(await screen.findByText('No active sessions')).toBeTruthy();
    expect(screen.queryByText('Loading sessions...')).toBeNull();
    expect(mockClient.GET).toHaveBeenCalledWith('/api/v1/admin/sessions', {
        params: { query: { status: 'active,idle' } },
    });
});

test('renders a populated list and URL-selected session detail', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: [activeSession],
        response: new Response(null, { status: 200 }),
    });

    render(
        <MemoryRouter
            initialEntries={[`/sessions?session=${activeSession.id}`]}
        >
            <SessionsPage />
        </MemoryRouter>,
    );

    expect(await screen.findAllByText(activeSession.user_id)).toHaveLength(2);
    expect(screen.getAllByText(activeSession.media_item_id)).toHaveLength(2);
    expect(screen.getAllByText(activeSession.variant_id)).toHaveLength(2);
    expect(screen.getAllByText('2:05')).toHaveLength(2);
    expect(screen.getByText('Session detail')).toBeTruthy();
    expect(screen.getByText('Status transitions')).toBeTruthy();
    expect(screen.getByText(/"position_seconds": 125/)).toBeTruthy();
});

test('refreshes sessions without leaving the page', async () => {
    vi.useFakeTimers();
    mockClient.GET.mockResolvedValueOnce({
        data: [],
        response: new Response(null, { status: 200 }),
    });
    mockClient.GET.mockResolvedValueOnce({
        data: [activeSession],
        response: new Response(null, { status: 200 }),
    });

    render(
        <MemoryRouter initialEntries={['/sessions']}>
            <SessionsPage />
        </MemoryRouter>,
    );

    await act(async () => {
        await Promise.resolve();
    });

    expect(screen.getByText('No active sessions')).toBeTruthy();

    await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
        await Promise.resolve();
    });

    expect(mockClient.GET).toHaveBeenCalledTimes(2);
    expect(screen.getByText(activeSession.user_id)).toBeTruthy();
    expect(
        screen.getByRole('heading', { name: 'Active sessions' }),
    ).toBeTruthy();
});
