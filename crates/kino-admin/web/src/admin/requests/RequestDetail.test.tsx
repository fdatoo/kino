import {
    cleanup,
    fireEvent,
    render,
    screen,
    waitFor,
} from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
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

import { RequestDetail } from './RequestDetail';

type RequestDetailResponse = components['schemas']['RequestDetail'];
type RequestState = components['schemas']['RequestState'];

const requestId = '018f0000-0000-7000-8000-000000000341';

beforeEach(() => {
    mockClient.DELETE.mockReset();
    mockClient.GET.mockReset();
    mockClient.POST.mockReset();
    mockClient.use.mockReset();
});

afterEach(() => {
    cleanup();
});

test('starts a manual import and shows the ingesting transition', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: requestDetail('fulfilling'),
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        data: {
            job_id: 'manual-job-1',
            path: '/srv/imports/movie.mkv',
            provider_id: 'manual-import',
            request: requestDetail('ingesting'),
        },
        response: new Response(null, { status: 200 }),
    });

    render(
        <MemoryRouter initialEntries={[`/requests/${requestId}`]}>
            <Routes>
                <Route path="/requests/:id" element={<RequestDetail />} />
            </Routes>
        </MemoryRouter>,
    );

    expect(await screen.findByText('The Matrix')).toBeTruthy();
    fireEvent.change(screen.getByLabelText('Filesystem path'), {
        target: { value: '/srv/imports/movie.mkv' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Start import' }));

    expect(await screen.findByText('Manual import started.')).toBeTruthy();
    expect(screen.getByText('Ingesting')).toBeTruthy();
    expect(screen.getByText('manual-job-1')).toBeTruthy();
    await waitFor(() => {
        expect(mockClient.POST).toHaveBeenCalledWith(
            '/api/v1/admin/requests/{id}/manual-import',
            {
                params: { path: { id: requestId } },
                body: { path: '/srv/imports/movie.mkv' },
            },
        );
    });
});

test('surfaces a missing manual import path inline', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: requestDetail('fulfilling'),
        response: new Response(null, { status: 200 }),
    });

    render(
        <MemoryRouter initialEntries={[`/requests/${requestId}`]}>
            <Routes>
                <Route path="/requests/:id" element={<RequestDetail />} />
            </Routes>
        </MemoryRouter>,
    );

    await screen.findByText('The Matrix');
    fireEvent.click(screen.getByRole('button', { name: 'Start import' }));

    expect((await screen.findByRole('alert')).textContent).toBe(
        'Enter a filesystem path.',
    );
    expect(mockClient.POST).not.toHaveBeenCalled();
});

test('resolves a request and renders ranked candidates', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: requestDetail('pending'),
        response: new Response(null, { status: 200 }),
    });
    mockClient.GET.mockResolvedValueOnce({
        data: requestDetail('resolved'),
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        data: {
            candidates: [
                {
                    canonical_identity_id: 'tmdb:movie:603',
                    popularity: 91.2,
                    rank: 1,
                    score: 0.997,
                    title: 'The Matrix',
                    year: 1999,
                },
                {
                    canonical_identity_id: 'tmdb:movie:604',
                    popularity: 62.5,
                    rank: 2,
                    score: 0.724,
                    title: 'The Matrix Reloaded',
                    year: 2003,
                },
            ],
        },
        response: new Response(null, { status: 200 }),
    });

    render(
        <MemoryRouter initialEntries={[`/requests/${requestId}`]}>
            <Routes>
                <Route path="/requests/:id" element={<RequestDetail />} />
            </Routes>
        </MemoryRouter>,
    );

    expect(await screen.findByText('The Matrix')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Resolve' }));

    expect(await screen.findByText('Resolve completed.')).toBeTruthy();
    expect(screen.getByText('tmdb:movie:603')).toBeTruthy();
    expect(screen.getByText('0.997')).toBeTruthy();
    await waitFor(() => {
        expect(mockClient.POST).toHaveBeenCalledWith(
            '/api/v1/requests/{id}/resolve',
            {
                params: { path: { id: requestId } },
            },
        );
    });
});

function requestDetail(state: RequestState): RequestDetailResponse {
    return {
        candidates: [],
        current_plan: null,
        identity_versions: [],
        plan_history: [],
        request: {
            created_at: '2026-05-11T12:00:00Z',
            failure_reason: null,
            id: requestId,
            plan_id: null,
            requester: 'anonymous',
            state,
            target: {
                canonical_identity_id:
                    'manual:movie:018f0000-0000-7000-8000-000000000999',
                raw_query: 'The Matrix',
            },
            updated_at: '2026-05-11T12:01:00Z',
        },
        status_events: [
            {
                actor: 'system',
                from_state: state === 'ingesting' ? 'fulfilling' : null,
                id: `${requestId}-event-${state}`,
                message:
                    state === 'ingesting'
                        ? 'manual import started'
                        : 'request accepted',
                occurred_at: '2026-05-11T12:01:00Z',
                request_id: requestId,
                to_state: state,
            },
        ],
    };
}
