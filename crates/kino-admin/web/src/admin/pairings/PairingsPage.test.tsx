import {
    act,
    cleanup,
    fireEvent,
    render,
    screen,
    waitFor,
} from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';

import type { components } from '../../api/schema';

const mockClearToken = vi.hoisted(() => vi.fn());
const mockSetToken = vi.hoisted(() => vi.fn());
const mockClient = vi.hoisted(() => ({
    DELETE: vi.fn(),
    GET: vi.fn(),
    POST: vi.fn(),
    use: vi.fn(),
}));

vi.mock('openapi-fetch', () => ({
    default: vi.fn(() => mockClient),
}));

vi.mock('../../auth/use-token', () => ({
    useToken: () => ({
        clearToken: mockClearToken,
        setToken: mockSetToken,
        token: 'admin-token',
    }),
}));

import { PairingsPage } from './PairingsPage';

type AdminPairingSummary = components['schemas']['AdminPairingSummary'];
type ApproveResult = {
    data: { pairing_id: string; token_preview: string };
    response: Response;
};

const livingRoomPairing = {
    code: '123456',
    created_at: '2026-05-11T12:00:00Z',
    device_name: 'Living Room TV',
    expires_at: '2026-05-11T12:05:00Z',
    pairing_id: '018f0000-0000-7000-8000-000000000001',
    platform: 'tvos',
} satisfies AdminPairingSummary;

const bedroomPairing = {
    code: '654321',
    created_at: '2026-05-11T12:01:00Z',
    device_name: 'Bedroom iPad',
    expires_at: '2026-05-11T12:06:00Z',
    pairing_id: '018f0000-0000-7000-8000-000000000002',
    platform: 'ios',
} satisfies AdminPairingSummary;

beforeEach(() => {
    mockClearToken.mockReset();
    mockSetToken.mockReset();
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

test('renders pending pairings', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-11T12:00:00Z'));
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [livingRoomPairing, bedroomPairing] },
        response: new Response(null, { status: 200 }),
    });

    renderPairingsPage();

    await act(async () => {
        await Promise.resolve();
    });

    expect(screen.getByText('Living Room TV')).toBeTruthy();
    expect(screen.getByText('Bedroom iPad')).toBeTruthy();
    expect(screen.getByText('tvos')).toBeTruthy();
    expect(screen.getByText('ios')).toBeTruthy();
    expect(screen.getByText('123456')).toBeTruthy();
    expect(screen.getByText('5m 00s')).toBeTruthy();
    expect(mockClient.GET).toHaveBeenCalledWith('/api/v1/admin/pairings');
});

test('renders empty state', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [] },
        response: new Response(null, { status: 200 }),
    });

    renderPairingsPage();

    await act(async () => {
        await Promise.resolve();
    });

    expect(screen.getByText('No pending pairings.')).toBeTruthy();
});

test('approves a pairing, shows the token preview, and refreshes the list', async () => {
    let resolveApprove: ((value: ApproveResult) => void) | null = null;
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [livingRoomPairing, bedroomPairing] },
        response: new Response(null, { status: 200 }),
    }).mockResolvedValueOnce({
        data: { pairings: [bedroomPairing] },
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockReturnValueOnce(
        new Promise((resolve) => {
            resolveApprove = resolve;
        }),
    );

    renderPairingsPage();

    await screen.findByText('Living Room TV');
    fireEvent.click(screen.getAllByRole('button', { name: 'Approve' })[0]);

    expect(
        buttonElement(
            screen.getByRole('button', {
                name: 'Approving...',
            }),
        ).disabled,
    ).toBe(true);
    expect(
        buttonElement(
            screen.getAllByRole('button', {
                name: 'Reject',
            })[0],
        ).disabled,
    ).toBe(true);
    expect(
        buttonElement(
            screen.getByRole('button', {
                name: 'Approve',
            }),
        ).disabled,
    ).toBe(false);

    await act(async () => {
        if (resolveApprove === null) {
            throw new Error('approve resolver not assigned');
        }

        resolveApprove({
            data: {
                pairing_id: livingRoomPairing.pairing_id,
                token_preview: 'abcxyz',
            },
            response: new Response(null, { status: 200 }),
        });
        await Promise.resolve();
    });

    expect(
        await screen.findByText(
            'Token ending ...abcxyz minted for Living Room TV.',
        ),
    ).toBeTruthy();
    expect(screen.queryByText('Living Room TV')).toBeNull();
    expect(screen.getByText('Bedroom iPad')).toBeTruthy();
    expect(mockClient.GET).toHaveBeenCalledTimes(2);
    expect(mockClient.POST).toHaveBeenCalledWith(
        '/api/v1/admin/pairings/{pairing_id}/approve',
        {
            params: {
                path: { pairing_id: livingRoomPairing.pairing_id },
            },
        },
    );
});

test('rejects a pairing, shows status, and refreshes the list', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [livingRoomPairing] },
        response: new Response(null, { status: 200 }),
    }).mockResolvedValueOnce({
        data: { pairings: [] },
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        response: new Response(null, { status: 204 }),
    });

    renderPairingsPage();

    await screen.findByText('Living Room TV');
    fireEvent.click(screen.getByRole('button', { name: 'Reject' }));

    expect(await screen.findByText('Pairing rejected.')).toBeTruthy();
    expect(await screen.findByText('No pending pairings.')).toBeTruthy();
    expect(mockClient.GET).toHaveBeenCalledTimes(2);
    expect(mockClient.POST).toHaveBeenCalledWith(
        '/api/v1/admin/pairings/{pairing_id}/reject',
        {
            params: {
                path: { pairing_id: livingRoomPairing.pairing_id },
            },
        },
    );
});

test('clears the token when listing pairings returns unauthorized', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: undefined,
        response: new Response(null, { status: 401 }),
    });

    renderPairingsPage();

    await waitFor(() => {
        expect(mockClearToken).toHaveBeenCalledTimes(1);
    });
});

test('clears the token when approving a pairing returns unauthorized', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [livingRoomPairing] },
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        data: undefined,
        response: new Response(null, { status: 401 }),
    });

    renderPairingsPage();

    await screen.findByText('Living Room TV');
    fireEvent.click(screen.getByRole('button', { name: 'Approve' }));

    await waitFor(() => {
        expect(mockClearToken).toHaveBeenCalledTimes(1);
    });
});

test('clears the token when rejecting a pairing returns unauthorized', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [livingRoomPairing] },
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        response: new Response(null, { status: 401 }),
    });

    renderPairingsPage();

    await screen.findByText('Living Room TV');
    fireEvent.click(screen.getByRole('button', { name: 'Reject' }));

    await waitFor(() => {
        expect(mockClearToken).toHaveBeenCalledTimes(1);
    });
});

test('auto-refreshes the list every five seconds', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-11T12:00:00Z'));
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [] },
        response: new Response(null, { status: 200 }),
    });
    mockClient.GET.mockResolvedValueOnce({
        data: { pairings: [livingRoomPairing] },
        response: new Response(null, { status: 200 }),
    });

    renderPairingsPage();

    await act(async () => {
        await Promise.resolve();
    });

    expect(screen.getByText('No pending pairings.')).toBeTruthy();

    await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
        await Promise.resolve();
    });

    expect(mockClient.GET).toHaveBeenCalledTimes(2);
    expect(screen.getByText('Living Room TV')).toBeTruthy();
});

function renderPairingsPage() {
    render(
        <MemoryRouter initialEntries={['/pairings']}>
            <PairingsPage />
        </MemoryRouter>,
    );
}

function buttonElement(element: HTMLElement): HTMLButtonElement {
    if (element instanceof HTMLButtonElement) {
        return element;
    }

    throw new Error('expected button element');
}
