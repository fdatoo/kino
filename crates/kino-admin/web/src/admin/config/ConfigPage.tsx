import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { apiClient } from '../../api/client';
import type { components } from '../../api/schema';
import { useToken } from '../../auth/use-token';
import { AdminHeader } from '../AdminHeader';

type AdminConfig = components['schemas']['AdminConfigResponse'];
type ConfigSource = components['schemas']['ConfigSource'];
type StringConfigValue = components['schemas']['StringConfigValue'];
type OptionalStringConfigValue =
    components['schemas']['OptionalStringConfigValue'];
type U32ConfigValue = components['schemas']['U32ConfigValue'];
type U64ConfigValue = components['schemas']['U64ConfigValue'];
type I32ConfigValue = components['schemas']['I32ConfigValue'];

export function ConfigPage() {
    const { clearToken } = useToken();
    const [config, setConfig] = useState<AdminConfig | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);

    const loadConfig = useCallback(async () => {
        setIsLoading(true);
        setError(null);

        try {
            const { data, response } = await apiClient.GET(
                '/api/v1/admin/config',
            );

            if (response.status === 401) {
                clearToken();
                return;
            }

            if (data === undefined) {
                setError('Config load failed.');
                return;
            }

            setConfig(data);
        } catch (err) {
            setError(
                err instanceof Error
                    ? `Config load failed: ${err.message}`
                    : 'Config load failed.',
            );
        } finally {
            setIsLoading(false);
        }
    }, [clearToken]);

    useEffect(() => {
        void loadConfig();
    }, [loadConfig]);

    return (
        <main className="admin-shell">
            <AdminHeader onSignOut={clearToken} title="Configuration" />

            {error !== null ? (
                <p className="status status-error" role="alert">
                    {error}
                </p>
            ) : null}

            {isLoading ? <p className="muted">Loading config...</p> : null}

            {config !== null ? <ConfigSections config={config} /> : null}
        </main>
    );
}

function ConfigSections({ config }: { config: AdminConfig }) {
    return (
        <>
            <ConfigSection title="Server">
                <dl className="config-list">
                    <ConfigRow
                        label="Database path"
                        field={config.database_path}
                    />
                    <ConfigRow label="Listen" field={config.server.listen} />
                    <ConfigRow
                        label="Public base URL"
                        field={config.server.public_base_url}
                    />
                    <ConfigRow
                        label="Reaper tick"
                        field={config.server.session_reaper.tick_seconds}
                        suffix="s"
                    />
                    <ConfigRow
                        label="Active to idle"
                        field={
                            config.server.session_reaper.active_to_idle_seconds
                        }
                        suffix="s"
                    />
                    <ConfigRow
                        label="Idle to ended"
                        field={
                            config.server.session_reaper.idle_to_ended_seconds
                        }
                        suffix="s"
                    />
                </dl>
            </ConfigSection>

            <ConfigSection title="Library">
                <dl className="config-list">
                    <ConfigRow label="Root" field={config.library.root} />
                    <ConfigRow
                        label="Canonical transfer"
                        field={config.library.canonical_transfer}
                    />
                    <OptionalConfigRow
                        label="Subtitle staging"
                        field={config.library.subtitle_staging_dir}
                    />
                </dl>
            </ConfigSection>

            <ConfigSection title="Providers">
                {config.providers.disc_rip === null ||
                config.providers.disc_rip === undefined ? (
                    <p className="muted">Disc rip provider not configured.</p>
                ) : (
                    <div className="config-subsection">
                        <h3>Disc rip</h3>
                        <dl className="config-list">
                            <ConfigRow
                                label="Path"
                                field={config.providers.disc_rip.path}
                            />
                            <ConfigRow
                                label="Preference"
                                field={config.providers.disc_rip.preference}
                            />
                        </dl>
                    </div>
                )}

                {config.providers.watch_folder === null ||
                config.providers.watch_folder === undefined ? (
                    <p className="muted">
                        Watch folder provider not configured.
                    </p>
                ) : (
                    <div className="config-subsection">
                        <h3>Watch folder</h3>
                        <dl className="config-list">
                            <ConfigRow
                                label="Path"
                                field={config.providers.watch_folder.path}
                            />
                            <ConfigRow
                                label="Preference"
                                field={config.providers.watch_folder.preference}
                            />
                            <ConfigRow
                                label="Stability"
                                field={
                                    config.providers.watch_folder
                                        .stability_seconds
                                }
                                suffix="s"
                            />
                        </dl>
                    </div>
                )}
            </ConfigSection>

            <ConfigSection title="TMDB">
                <dl className="config-list">
                    <MaskedConfigRow
                        label="API key"
                        field={config.tmdb.api_key}
                    />
                    <ConfigRow
                        label="Max requests per second"
                        field={config.tmdb.max_requests_per_second}
                    />
                </dl>
            </ConfigSection>

            <ConfigSection title="Log">
                <dl className="config-list">
                    <ConfigRow label="Level" field={config.log.level} />
                    <ConfigRow label="Format" field={config.log.format} />
                </dl>
            </ConfigSection>
        </>
    );
}

function ConfigSection({
    children,
    title,
}: {
    children: ReactNode;
    title: string;
}) {
    return (
        <section className="section-block">
            <details className="config-section" open>
                <summary>
                    <h2>{title}</h2>
                </summary>
                <div className="config-section-body">{children}</div>
            </details>
        </section>
    );
}

function ConfigRow({
    field,
    label,
    suffix = '',
}: {
    field: I32ConfigValue | StringConfigValue | U32ConfigValue | U64ConfigValue;
    label: string;
    suffix?: string;
}) {
    return (
        <ConfigValueRow
            label={label}
            source={field.source}
            value={`${field.value}${suffix}`}
        />
    );
}

function OptionalConfigRow({
    field,
    label,
}: {
    field: OptionalStringConfigValue;
    label: string;
}) {
    return (
        <ConfigValueRow
            label={label}
            source={field.source}
            value={field.value ?? '(not set)'}
        />
    );
}

function MaskedConfigRow({
    field,
    label,
}: {
    field: OptionalStringConfigValue;
    label: string;
}) {
    return (
        <ConfigValueRow
            label={label}
            source={field.source}
            value={
                field.value === null || field.value === undefined
                    ? '(not set)'
                    : '(set)'
            }
        />
    );
}

function ConfigValueRow({
    label,
    source,
    value,
}: {
    label: string;
    source: ConfigSource;
    value: string;
}) {
    return (
        <div className="config-row">
            <dt>{label}</dt>
            <dd>
                <span>{value}</span>
                <span className="source-pill">{sourceLabel(source)}</span>
            </dd>
        </div>
    );
}

function sourceLabel(source: ConfigSource): string {
    switch (source) {
        case 'env':
            return 'env';
        case 'file':
            return 'file';
        case 'default':
            return 'default';
        case 'unknown':
            return 'unknown';
    }
}
