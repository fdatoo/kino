import { useCallback, useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';

type CatalogMediaItem = components['schemas']['CatalogMediaItem'];
type CatalogSubtitleTrack = components['schemas']['CatalogSubtitleTrack'];

type ReocrTrackState =
    | { status: 'running' }
    | { jobId: string; status: 'succeeded' }
    | { error: string; status: 'failed' };

export function ItemDetailPage() {
    const { id } = useParams();
    const { clearToken } = useToken();
    const [item, setItem] = useState<CatalogMediaItem | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);
    const [reocrTracks, setReocrTracks] = useState<
        Record<number, ReocrTrackState>
    >({});

    const loadItem = useCallback(async () => {
        if (id === undefined) {
            setError('Item id is missing.');
            setIsLoading(false);
            return;
        }

        setIsLoading(true);
        setError(null);

        try {
            const {
                data,
                error: apiError,
                response,
            } = await apiClient.GET('/api/v1/library/items/{id}', {
                params: { path: { id } },
            });

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError(apiErrorMessage(apiError, 'Item load failed.'));
                return;
            }

            setItem(data);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Item load failed: ${err.message}`
                    : 'Item load failed.',
            );
        } finally {
            setIsLoading(false);
        }
    }, [clearToken, id]);

    useEffect(() => {
        void loadItem();
    }, [loadItem]);

    async function handleReocr(track: CatalogSubtitleTrack) {
        if (id === undefined) {
            setReocrTrackState(track.track_index, {
                error: 'Item id is missing.',
                status: 'failed',
            });
            return;
        }

        setReocrTrackState(track.track_index, { status: 'running' });

        try {
            const {
                data,
                error: apiError,
                response,
            } = await apiClient.POST(
                '/api/v1/admin/items/{id}/subtitles/{track}/re-ocr',
                {
                    params: {
                        path: {
                            id,
                            track: track.track_index,
                        },
                    },
                },
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setReocrTrackState(track.track_index, {
                    error: apiErrorMessage(apiError, 'Re-OCR failed.'),
                    status: 'failed',
                });
                return;
            }

            setReocrTrackState(track.track_index, {
                jobId: data.job_id,
                status: 'succeeded',
            });
            await loadItem();
        } catch (err) {
            setReocrTrackState(track.track_index, {
                error:
                    err instanceof Error
                        ? `Re-OCR failed: ${err.message}`
                        : 'Re-OCR failed.',
                status: 'failed',
            });
        }
    }

    function setReocrTrackState(
        trackIndex: number,
        state: ReocrTrackState,
    ): void {
        setReocrTracks((current) => ({
            ...current,
            [trackIndex]: state,
        }));
    }

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Catalog item" />

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}

            {isLoading ? <p className="muted">Loading item...</p> : null}

            {item !== null ? (
                <>
                    <ItemSummary item={item} />
                    <SubtitleTracks
                        onReocr={(track) => {
                            void handleReocr(track);
                        }}
                        reocrTracks={reocrTracks}
                        tracks={item.subtitle_tracks}
                    />
                </>
            ) : null}
        </main>
    );
}

function ItemSummary({ item }: { item: CatalogMediaItem }) {
    return (
        <section className="section-block" aria-labelledby="item-title">
            <p className="eyebrow">{mediaKindLabel(item.media_kind)}</p>
            <h2 id="item-title">{item.title ?? item.id}</h2>
            <dl className="item-facts">
                <Fact label="Item id" value={item.id} />
                <Fact label="Year" value={formatOptional(item.year)} />
                <Fact
                    label="Release"
                    value={formatOptional(item.release_date)}
                />
                <Fact
                    label="Sources"
                    value={item.source_files.length.toString()}
                />
                <Fact
                    label="Variants"
                    value={item.variants.length.toString()}
                />
            </dl>
        </section>
    );
}

function SubtitleTracks({
    onReocr,
    reocrTracks,
    tracks,
}: {
    onReocr: (track: CatalogSubtitleTrack) => void;
    reocrTracks: Record<number, ReocrTrackState>;
    tracks: CatalogSubtitleTrack[];
}) {
    return (
        <section className="section-block" aria-labelledby="subtitles-title">
            <h2 id="subtitles-title">Subtitle tracks</h2>
            <div className="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th>Track</th>
                            <th>Label</th>
                            <th>Language</th>
                            <th>Format</th>
                            <th>Provenance</th>
                            <th>Forced</th>
                            <th>Sidecar</th>
                            <th aria-label="Actions" />
                        </tr>
                    </thead>
                    <tbody>
                        {tracks.length === 0 ? (
                            <tr>
                                <td colSpan={8}>No subtitle tracks found.</td>
                            </tr>
                        ) : (
                            tracks.map((track) => (
                                <SubtitleTrackRow
                                    key={track.id}
                                    onReocr={onReocr}
                                    reocrState={reocrTracks[track.track_index]}
                                    track={track}
                                />
                            ))
                        )}
                    </tbody>
                </table>
            </div>
        </section>
    );
}

function SubtitleTrackRow({
    onReocr,
    reocrState,
    track,
}: {
    onReocr: (track: CatalogSubtitleTrack) => void;
    reocrState: ReocrTrackState | undefined;
    track: CatalogSubtitleTrack;
}) {
    const isRunning = reocrState?.status === 'running';

    return (
        <tr>
            <td>{track.track_index}</td>
            <td>{track.label}</td>
            <td>{track.language}</td>
            <td>{track.format}</td>
            <td>
                <span className="source-pill">{provenanceLabel(track)}</span>
            </td>
            <td>{track.forced ? 'yes' : 'no'}</td>
            <td>
                <code>{track.id}</code>
            </td>
            <td className="table-action">
                {track.provenance === 'ocr' ? (
                    <div className="track-action">
                        <button
                            disabled={isRunning}
                            onClick={() => onReocr(track)}
                            type="button"
                        >
                            {isRunning ? 'Re-OCR running...' : 'Re-OCR'}
                        </button>
                        <ReocrStatus state={reocrState} />
                    </div>
                ) : (
                    <span className="muted-inline">Text track</span>
                )}
            </td>
        </tr>
    );
}

function ReocrStatus({ state }: { state: ReocrTrackState | undefined }) {
    if (state === undefined) {
        return null;
    }

    switch (state.status) {
        case 'running':
            return (
                <p className="track-status" role="status">
                    Re-OCR running...
                </p>
            );
        case 'succeeded':
            return (
                <p className="track-status" role="status">
                    Re-OCR complete. Job {state.jobId}
                </p>
            );
        case 'failed':
            return (
                <p className="track-status status-error" role="alert">
                    {state.error}
                </p>
            );
    }
}

function Fact({ label, value }: { label: string; value: string }) {
    return (
        <div className="config-row">
            <dt>{label}</dt>
            <dd>{value}</dd>
        </div>
    );
}

function apiErrorMessage(value: unknown, fallback: string): string {
    if (typeof value !== 'object' || value === null) {
        return fallback;
    }

    const record = value as Record<string, unknown>;
    return typeof record.error === 'string' ? record.error : fallback;
}

function formatOptional(value: number | string | null | undefined): string {
    if (value === null || value === undefined || value === '') {
        return '--';
    }

    return value.toString();
}

function mediaKindLabel(kind: CatalogMediaItem['media_kind']): string {
    switch (kind) {
        case 'movie':
            return 'Movie';
        case 'tv_episode':
            return 'TV episode';
        case 'personal':
            return 'Personal';
    }
}

function provenanceLabel(track: CatalogSubtitleTrack): string {
    return track.provenance === 'ocr' ? 'ocr' : 'text';
}
