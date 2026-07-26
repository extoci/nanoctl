function productionHttpsUrl(name: string, value: string | undefined): URL {
  if (!value) throw new Error(`${name} is required for a production build`);
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error(`${name} must be an absolute URL`);
  }
  if (
    url.protocol !== "https:" ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  ) {
    throw new Error(`${name} must be a credential-free HTTPS origin`);
  }
  const hostname = url.hostname.toLowerCase();
  if (
    hostname === "localhost" ||
    hostname === "0.0.0.0" ||
    hostname === "::1" ||
    hostname === "[::1]" ||
    hostname.startsWith("127.")
  ) {
    throw new Error(`${name} cannot use a loopback host`);
  }
  return url;
}

export function validateProductionEnvironment(
  environment: Readonly<Record<string, string | undefined>>,
): void {
  if (environment.NANOCTL_E2E_FIXTURES) {
    throw new Error("NANOCTL_E2E_FIXTURES must not be set for a production build");
  }
  const convex = productionHttpsUrl("NEXT_PUBLIC_CONVEX_URL", environment.NEXT_PUBLIC_CONVEX_URL);
  if (!convex.hostname.endsWith(".convex.cloud")) {
    throw new Error("NEXT_PUBLIC_CONVEX_URL must use a Convex cloud deployment");
  }
  productionHttpsUrl("NEXT_PUBLIC_APP_ORIGIN", environment.NEXT_PUBLIC_APP_ORIGIN);
}

if (import.meta.main) {
  validateProductionEnvironment(process.env);
  console.log("Production web environment is valid.");
}
