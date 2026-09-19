import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// This package ships source, not a build — the apps bundle it. The only thing
// Vite does here is `npm run check:inert`, which SSR-builds Answer.inert-check.tsx
// and runs the output under node.
//
// The react plugin is present so that JSX is transformed exactly as it is in
// apps/warden. Without it the check would depend on esbuild inferring the JSX
// runtime from tsconfig, which is a difference between the two build paths that
// nothing would announce.
export default defineConfig({
  plugins: [react()],
});
