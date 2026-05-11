import { describe, expect, test } from 'vitest';

import {
    ADMIN_TOKEN_STORAGE_KEY,
    TokenStore,
    type TokenStorage,
} from './token-store';

class MemoryStorage implements TokenStorage {
    readonly #values = new Map<string, string>();

    getItem(key: string): string | null {
        return this.#values.get(key) ?? null;
    }

    setItem(key: string, value: string): void {
        this.#values.set(key, value);
    }

    removeItem(key: string): void {
        this.#values.delete(key);
    }
}

describe('TokenStore', () => {
    test('sets and gets the admin token', () => {
        const storage = new MemoryStorage();
        const store = new TokenStore(storage);

        store.set('bootstrap-token');

        expect(store.get()).toBe('bootstrap-token');
        expect(storage.getItem(ADMIN_TOKEN_STORAGE_KEY)).toBe(
            'bootstrap-token',
        );
    });

    test('clears the admin token', () => {
        const store = new TokenStore(new MemoryStorage());

        store.set('bootstrap-token');
        store.clear();

        expect(store.get()).toBeNull();
    });
});
