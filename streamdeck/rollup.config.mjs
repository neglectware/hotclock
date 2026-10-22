import commonjs from "@rollup/plugin-commonjs";
import nodeResolve from "@rollup/plugin-node-resolve";
import terser from "@rollup/plugin-terser";
import typescript from "@rollup/plugin-typescript";
import url from "node:url";

const isWatching = !!process.env.ROLLUP_WATCH;
const sdPlugin = "com.neglectware.hotclock.sdPlugin";

/** @type {import('rollup').RollupOptions} */
const config = {
  input: "src/plugin.ts",
  output: {
    file: `${sdPlugin}/bin/plugin.js`,
    format: "esm",
    sourcemap: isWatching,
    sourcemapPathTransform: (relativeSourcePath, sourcemapPath) =>
      url.pathToFileURL(new URL(relativeSourcePath, new URL(sourcemapPath, "file:///")).pathname).href,
  },
  external: ["node:*"],
  plugins: [
    typescript({ tsconfig: "./tsconfig.json", noEmit: false, outDir: "com.neglectware.hotclock.sdPlugin/bin", include: ["src/**/*.ts"] }),
    nodeResolve({ browser: false, exportConditions: ["node"], preferBuiltins: true }),
    commonjs(),
    !isWatching && terser(),
    {
      name: "emit-module-package-file",
      generateBundle() {
        this.emitFile({ fileName: "package.json", source: `{ "type": "module" }`, type: "asset" });
      },
    },
  ],
};

export default config;
