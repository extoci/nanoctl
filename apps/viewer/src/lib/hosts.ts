import { assertHostsResponse, type HostsResponse } from "@nanoctl/protocol";

export async function fetchHosts(
  fetcher: (url: string) => Promise<Response> = fetch,
): Promise<HostsResponse> {
  const response = await fetcher("/v1/hosts");
  if (!response.ok) throw new Error("Host discovery unavailable");
  const data: unknown = await response.json();
  assertHostsResponse(data);
  data.hosts.sort((a, b) => a.hostName.localeCompare(b.hostName));
  return data;
}
