import createClient, { type ClientOptions } from 'openapi-fetch';

import type { paths } from './schema';

export type ApiClientOptions = ClientOptions;

export function createApiClient(options: ApiClientOptions = {}) {
    return createClient<paths>(options);
}

export const apiClient = createApiClient();
