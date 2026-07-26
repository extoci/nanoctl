import { notFound } from "next/navigation";
import { E2eDashboardFixture } from "../../../components/e2e-dashboard-fixture";

const STATES = new Set(["loading", "empty", "mixed", "errors"]);

export default async function E2eDashboardPage({
  searchParams,
}: {
  searchParams: Promise<{ state?: string }>;
}) {
  if (process.env.NANOCTL_E2E_FIXTURES !== "1") notFound();
  const requested = (await searchParams).state ?? "mixed";
  const state = STATES.has(requested)
    ? (requested as "loading" | "empty" | "mixed" | "errors")
    : "mixed";
  return <E2eDashboardFixture state={state} />;
}
