# Next.js Rookhold starter

This starter targets the upcoming `rookhold@0.8.0` npm package. Until that
package is public, install the packed SDK from the Rookhold source checkout.

```bash
npm install
npm run dev
```

Set `ROOKHOLD_BASE_URL` and `ROOKHOLD_API_KEY` in `.env.local`. The API route
requires gVisor and clamps each job to three seconds and 128 MB. Add sign-in,
per-user quotas, request logging, and abuse controls before production use.
