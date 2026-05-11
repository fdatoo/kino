import { expect, test } from 'vitest';

import type { components } from './schema';

test('generated OpenAPI schema includes component schemas', () => {
    const schemaNames = [
        'CatalogListPage',
    ] satisfies readonly (keyof components['schemas'])[];

    expect(schemaNames.length).toBeGreaterThan(0);
});
