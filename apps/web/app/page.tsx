import { AuthGate } from "../components/auth-gate";
import { DeviceDashboard } from "../components/device-dashboard";

export default function Home() {
  return (
    <AuthGate>
      <DeviceDashboard />
    </AuthGate>
  );
}
