import createClient, {
    type ClientOptions,
    type Middleware,
} from 'openapi-fetch';

import { adminTokenStore, emitTokenChanged } from '../auth/token-store';
import type { paths } from './schema';

export type ApiClientOptions = ClientOptions;

const authMiddleware: Middleware = {
    onRequest({ request }) {
        const token = adminTokenStore.get();

        if (token === null) {
            return undefined;
        }

        const headers = new Headers(request.headers);
        headers.set('authorization', `Bearer ${token}`);

        return new Request(request, { headers });
    },
    onResponse({ response }) {
        if (response.status !== 401) {
            return undefined;
        }

        adminTokenStore.clear();
        emitTokenChanged();
        return undefined;
    },
};

export function createApiClient(options: ApiClientOptions = {}) {
    const client = createClient<paths>(options);
    client.use(authMiddleware);
    return client;
}

export const apiClient = createApiClient();
