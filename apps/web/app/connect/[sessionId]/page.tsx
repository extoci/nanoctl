import { AuthGate } from "../../../components/auth-gate";
import { RemoteViewer } from "../../../components/remote-viewer";

export default async function ConnectPage({ params }: { params: Promise<{ sessionId: string }> }) {
  const { sessionId } = await params;
  return (
    <AuthGate>
      <RemoteViewer sessionId={sessionId} />
    </AuthGate>
  );
}
