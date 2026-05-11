import { cleanup, render, screen } from '@testing-library/react';
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

import { ConfigPage } from './ConfigPage';

type AdminConfig = components['schemas']['AdminConfigResponse'];

beforeEach(() => {
    mockClient.DELETE.mockReset();
    mockClient.GET.mockReset();
    mockClient.POST.mockReset();
    mockClient.use.mockReset();
});

afterEach(() => {
    cleanup();
});

test('renders resolved config with masked secret state', async () => {
    const config = {
        database_path: { value: '/var/lib/kino/kino.db', source: 'env' },
        library: {
            canonical_transfer: { value: 'hard_link', source: 'default' },
            root: { value: '/srv/media', source: 'file' },
            subtitle_staging_dir: { value: null, source: 'default' },
        },
        log: {
            format: { value: 'json', source: 'env' },
            level: { value: 'debug,kino=trace', source: 'env' },
        },
        ocr: {
            language: { value: 'eng', source: 'default' },
            tesseract_path: { value: 'tesseract', source: 'default' },
        },
        providers: {
            disc_rip: {
                path: { value: '/srv/kino/rips', source: 'file' },
                preference: { value: 10, source: 'file' },
            },
            watch_folder: null,
        },
        server: {
            listen: { value: '127.0.0.1:7777', source: 'default' },
            public_base_url: {
                value: 'https://kino.example.test',
                source: 'env',
            },
            session_reaper: {
                active_to_idle_seconds: { value: 60, source: 'default' },
                idle_to_ended_seconds: { value: 300, source: 'default' },
                tick_seconds: { value: 30, source: 'default' },
            },
        },
        tmdb: {
            api_key: { value: '***', source: 'env' },
            max_requests_per_second: { value: 7, source: 'file' },
        },
    } satisfies AdminConfig;
    mockClient.GET.mockResolvedValueOnce({
        data: config,
        response: new Response(null, { status: 200 }),
    });

    render(
        <MemoryRouter>
            <ConfigPage />
        </MemoryRouter>,
    );

    expect(await screen.findByText('/srv/media')).toBeTruthy();
    expect(screen.getByText('/var/lib/kino/kino.db')).toBeTruthy();
    expect(screen.getByText('Disc rip')).toBeTruthy();
    expect(screen.getByText('/srv/kino/rips')).toBeTruthy();
    expect(
        screen.getByText('Watch folder provider not configured.'),
    ).toBeTruthy();
    expect(screen.getByText('(set)')).toBeTruthy();
    expect(screen.getByText('json')).toBeTruthy();
    expect(screen.queryByText('super-secret-tmdb-key')).toBeNull();
    expect(mockClient.GET).toHaveBeenCalledWith('/api/v1/admin/config');
});
