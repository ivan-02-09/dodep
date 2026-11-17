"use client";

import { Suspense, useEffect } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { useAuthStore } from "@/stores/auth-store";

function GoogleCallbackInner() {
  const router = useRouter();
  const params = useSearchParams();
  const { fetchUser } = useAuthStore();

  useEffect(() => {
    const errorCode = params.get("error_code");
    const errorMessage = params.get("error_message");

    if (errorCode) {
      router.replace(
        `/login?error=${encodeURIComponent(errorCode)}&message=${encodeURIComponent(errorMessage || "OAuth failed")}`,
      );
      return;
    }

    // The auth service already set the HttpOnly session cookies on the redirect
    // that landed here, so there is nothing to read from the URL: just confirm
    // the session works and move on.
    fetchUser()
      .then(() => router.replace("/sportsbook"))
      .catch(() => router.replace("/login?error=AUTH_OAUTH_USER_FETCH_FAILED"));
  }, [fetchUser, params, router]);

  return (
    <div className="min-h-[calc(100vh-4rem)] flex items-center justify-center px-4">
      <div className="card !p-8 text-center">
        <h1 className="text-2xl font-bold text-white">Signing you in...</h1>
        <p className="mt-2 text-gray-400">
          Google authorization complete, redirecting to sportsbook.
        </p>
      </div>
    </div>
  );
}

export default function GoogleCallbackPage() {
  // useSearchParams() opts the subtree out of static prerendering, so Next
  // refuses to export this page unless the part that reads the query string
  // sits inside a Suspense boundary. Wrapping the inner component keeps the
  // reading hook where Next expects it and lets the shell prerender with the
  // same placeholder the page always showed.
  return (
    <Suspense
      fallback={
        <div className="min-h-[calc(100vh-4rem)] flex items-center justify-center px-4">
          <div className="card !p-8 text-center">
            <h1 className="text-2xl font-bold text-white">Signing you in...</h1>
            <p className="mt-2 text-gray-400">
              Google authorization complete, redirecting to sportsbook.
            </p>
          </div>
        </div>
      }
    >
      <GoogleCallbackInner />
    </Suspense>
  );
}
