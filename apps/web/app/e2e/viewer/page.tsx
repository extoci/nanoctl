import { notFound } from "next/navigation";
import { E2eRemoteViewerFixture } from "../../../components/e2e-remote-viewer-fixture";

export default async function E2eViewerPage({
  searchParams,
}: {
  searchParams: Promise<{ state?: string }>;
}) {
  if (process.env.NANOCTL_E2E_FIXTURES !== "1") notFound();
  const terminal = (await searchParams).state === "ended";
  return <E2eRemoteViewerFixture terminal={terminal} />;
}
