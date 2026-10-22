import { defineConfig } from "vitest/config";
export default defineConfig({
  test: { include: ["test/integration/**/*.test.ts"], environment: "node", testTimeout: 90_000, hookTimeout: 120_000, fileParallelism: false },
});
