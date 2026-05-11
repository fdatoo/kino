import type { components } from '../../api/schema';

type RequestState = components['schemas']['RequestState'];

const requestStates = [
    'pending',
    'needs_disambiguation',
    'resolved',
    'planning',
    'fulfilling',
    'ingesting',
    'satisfied',
    'failed',
    'cancelled',
] satisfies RequestState[];

export function allRequestStates(): readonly RequestState[] {
    return requestStates;
}

export function formatState(state: RequestState): string {
    return state
        .split('_')
        .map((word) => `${word[0].toUpperCase()}${word.slice(1)}`)
        .join(' ');
}

export function formatTimestamp(value: string | null | undefined): string {
    if (value === null || value === undefined) {
        return '--';
    }

    return new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
    }).format(new Date(value));
}

export function apiErrorMessage(error: unknown, fallback: string): string {
    if (
        typeof error === 'object' &&
        error !== null &&
        'error' in error &&
        typeof error.error === 'string'
    ) {
        return error.error;
    }

    return fallback;
}

export function isRequestState(value: string | null): value is RequestState {
    return requestStates.some((state) => state === value);
}
