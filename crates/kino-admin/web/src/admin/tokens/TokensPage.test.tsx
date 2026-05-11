import {
    cleanup,
    fireEvent,
    render,
    screen,
    waitFor,
} from '@testing-library/react';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { MemoryRouter } from 'react-router-dom';

const mockClient = vi.hoisted(() => ({
    DELETE: vi.fn(),
    GET: vi.fn(),
    POST: vi.fn(),
    use: vi.fn(),
}));

vi.mock('openapi-fetch', () => ({
    default: vi.fn(() => mockClient),
}));

import { TokensPage } from './TokensPage';

let clipboardWriteText: ReturnType<typeof vi.fn>;

beforeEach(() => {
    mockClient.DELETE.mockReset();
    mockClient.GET.mockReset();
    mockClient.POST.mockReset();
    mockClient.use.mockReset();

    clipboardWriteText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        value: {
            writeText: clipboardWriteText,
        },
    });
});

afterEach(() => {
    cleanup();
});

test('displays a freshly minted plaintext token once', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: { tokens: [] },
        response: new Response(null, { status: 200 }),
    }).mockResolvedValueOnce({
        data: {
            tokens: [
                {
                    created_at: '2026-05-11T12:00:00Z',
                    label: 'Desk browser',
                    last_seen_at: null,
                    revoked_at: null,
                    token_id: '018f0000-0000-7000-8000-000000000001',
                },
            ],
        },
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        data: {
            created_at: '2026-05-11T12:00:00Z',
            label: 'Desk browser',
            token: 'plain-token-once',
            token_id: '018f0000-0000-7000-8000-000000000001',
        },
        response: new Response(null, { status: 201 }),
    });

    render(
        <MemoryRouter>
            <TokensPage />
        </MemoryRouter>,
    );

    await screen.findByText('No tokens found.');
    fireEvent.change(screen.getByLabelText('Label'), {
        target: { value: 'Desk browser' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Mint token' }));

    expect(await screen.findByText('plain-token-once')).toBeTruthy();
    expect(mockClient.POST).toHaveBeenCalledWith('/api/v1/admin/tokens', {
        body: { label: 'Desk browser' },
    });

    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));

    await waitFor(() => {
        expect(screen.queryByText('plain-token-once')).toBeNull();
    });
    expect(clipboardWriteText).toHaveBeenCalledWith('plain-token-once');
});
