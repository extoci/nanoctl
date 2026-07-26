import { AuthGate } from "../../components/auth-gate";
import { DeviceDashboard } from "../../components/device-dashboard";

export default function DashboardPage() {
  return (
    <AuthGate>
      <DeviceDashboard />
    </AuthGate>
  );
}
