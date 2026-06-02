# spoti-dump frontend

This React application is the local source-of-truth console served by the Rust runtime at `/app/`.

## Development

```sh
npm ci
npm run lint
npm run build
```

The production bundle is written to `dist/`. Release archives keep `frontend/dist/` next to the `spoti-dump` executable so the runtime can serve the compiled assets without requiring Node.js on the user's machine.

The frontend talks to the local `/api/*` routes. Provider credentials are submitted only to the local Rust process and are stored in `dump/runtime.db`, separately from the canonical music library.
