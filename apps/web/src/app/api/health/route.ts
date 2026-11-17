/**
 * Liveness probe for the Next.js container.
 *
 * docker-compose.prod.yml and the Kubernetes manifests probe /api/health; the
 * route did not exist, so the container answered 404 and curl -fsS failed,
 * leaving `web` permanently unhealthy and `nginx` (depends_on web) never
 * started. Keep this route dependency-free: it must not touch the backend, or
 * a backend outage would restart the frontend.
 */
export const dynamic = "force-dynamic";

export function GET() {
  return Response.json(
    { status: "ok", service: "web" },
    { status: 200, headers: { "cache-control": "no-store" } }
  );
}
