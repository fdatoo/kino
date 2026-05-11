import {
    cleanup,
    fireEvent,
    render,
    screen,
    waitFor,
} from '@testing-library/react';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { MemoryRouter, Route, Routes } from 'react-router-dom';

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

import { ItemDetailPage } from './ItemDetailPage';

type CatalogMediaItem = components['schemas']['CatalogMediaItem'];
type ReocrResponse = {
    data: { job_id: string };
    response: Response;
};

beforeEach(() => {
    mockClient.DELETE.mockReset();
    mockClient.GET.mockReset();
    mockClient.POST.mockReset();
    mockClient.use.mockReset();
});

afterEach(() => {
    cleanup();
});

test('runs re-ocr for an ocr subtitle track and refreshes the item detail', async () => {
    let resolveReocr: (value: ReocrResponse) => void = () => {
        throw new Error('re-ocr resolver was not initialized');
    };
    const reocrRequest = new Promise<ReocrResponse>((resolve) => {
        resolveReocr = resolve;
    });

    mockClient.GET.mockResolvedValueOnce({
        data: catalogItem({
            ocrSidecarId: '018f0000-0000-7000-8000-000000000101',
        }),
        response: new Response(null, { status: 200 }),
    }).mockResolvedValueOnce({
        data: catalogItem({
            ocrSidecarId: '018f0000-0000-7000-8000-000000000202',
        }),
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockReturnValueOnce(reocrRequest);

    render(
        <MemoryRouter
            initialEntries={['/items/018f0000-0000-7000-8000-000000000001']}
        >
            <Routes>
                <Route path="/items/:id" element={<ItemDetailPage />} />
            </Routes>
        </MemoryRouter>,
    );

    expect(await screen.findByText('JPN (OCR)')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Re-OCR' }));

    expect(
        await screen.findByRole('button', { name: 'Re-OCR running...' }),
    ).toHaveProperty('disabled', true);
    expect(screen.getByRole('status').textContent).toBe('Re-OCR running...');

    resolveReocr({
        data: { job_id: '018f0000-0000-7000-8000-000000000303' },
        response: new Response(null, { status: 202 }),
    });

    expect(
        await screen.findByText('018f0000-0000-7000-8000-000000000202'),
    ).toBeTruthy();
    expect(
        screen.queryByText('018f0000-0000-7000-8000-000000000101'),
    ).toBeNull();
    expect(
        screen.getByText(
            'Re-OCR complete. Job 018f0000-0000-7000-8000-000000000303',
        ),
    ).toBeTruthy();
    await waitFor(() => {
        expect(mockClient.POST).toHaveBeenCalledWith(
            '/api/v1/admin/items/{id}/subtitles/{track}/re-ocr',
            {
                params: {
                    path: {
                        id: '018f0000-0000-7000-8000-000000000001',
                        track: 4,
                    },
                },
            },
        );
    });
});

test('renders re-ocr api error text verbatim for the failed track', async () => {
    mockClient.GET.mockResolvedValueOnce({
        data: catalogItem({
            ocrSidecarId: '018f0000-0000-7000-8000-000000000101',
        }),
        response: new Response(null, { status: 200 }),
    });
    mockClient.POST.mockResolvedValueOnce({
        error: {
            error: 'ocr command /usr/bin/tesseract exited with status 1: bad pixels',
        },
        response: new Response(null, { status: 500 }),
    });

    render(
        <MemoryRouter
            initialEntries={['/items/018f0000-0000-7000-8000-000000000001']}
        >
            <Routes>
                <Route path="/items/:id" element={<ItemDetailPage />} />
            </Routes>
        </MemoryRouter>,
    );

    expect(await screen.findByText('JPN (OCR)')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Re-OCR' }));

    expect(
        await screen.findByText(
            'ocr command /usr/bin/tesseract exited with status 1: bad pixels',
        ),
    ).toBeTruthy();
});

function catalogItem({
    ocrSidecarId,
}: {
    ocrSidecarId: string;
}): CatalogMediaItem {
    return {
        artwork: {
            backdrop: null,
            logo: null,
            poster: null,
        },
        canonical_identity_id: 'tmdb:movie:603',
        cast: [],
        created_at: '2026-05-11T12:00:00Z',
        description: 'A hacker sees the source.',
        id: '018f0000-0000-7000-8000-000000000001',
        media_kind: 'movie',
        release_date: '1999-03-31',
        season_number: null,
        source_files: [],
        subtitle_tracks: [
            {
                forced: false,
                format: 'srt',
                id: '018f0000-0000-7000-8000-000000000100',
                label: 'ENG',
                language: 'eng',
                provenance: 'text',
                track_index: 3,
            },
            {
                forced: false,
                format: 'json',
                id: ocrSidecarId,
                label: 'JPN (OCR)',
                language: 'jpn',
                provenance: 'ocr',
                track_index: 4,
            },
        ],
        title: 'The Matrix',
        updated_at: '2026-05-11T12:00:00Z',
        variants: [],
        year: 1999,
    };
}
