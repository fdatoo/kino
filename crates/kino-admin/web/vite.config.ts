import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
    base: '/admin/',
    plugins: [react()],
    test: {
        environment: 'jsdom',
    },
});
