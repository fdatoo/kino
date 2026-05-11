export const ADMIN_TOKEN_STORAGE_KEY = 'kino_admin_token';
export const TOKEN_CHANGED_EVENT = 'kino-admin-token-changed';

export interface TokenStorage {
    getItem(key: string): string | null;
    setItem(key: string, value: string): void;
    removeItem(key: string): void;
}

class MemoryTokenStorage implements TokenStorage {
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

export class TokenStore {
    readonly #storage: TokenStorage;
    readonly #key: string;

    constructor(
        storage: TokenStorage = defaultTokenStorage(),
        key = ADMIN_TOKEN_STORAGE_KEY,
    ) {
        this.#storage = storage;
        this.#key = key;
    }

    get(): string | null {
        const token = this.#storage.getItem(this.#key);
        return token === '' ? null : token;
    }

    set(token: string): void {
        this.#storage.setItem(this.#key, token);
    }

    clear(): void {
        this.#storage.removeItem(this.#key);
    }
}

export const adminTokenStore = new TokenStore();

export function emitTokenChanged(): void {
    window.dispatchEvent(new Event(TOKEN_CHANGED_EVENT));
}

function defaultTokenStorage(): TokenStorage {
    if (
        typeof window !== 'undefined' &&
        typeof window.localStorage?.getItem === 'function' &&
        typeof window.localStorage.setItem === 'function' &&
        typeof window.localStorage.removeItem === 'function'
    ) {
        return window.localStorage;
    }

    return new MemoryTokenStorage();
}
