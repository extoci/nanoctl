import { expect, test } from "bun:test";
import { fetchHosts } from "./hosts";

test("fetches same-origin hosts, validates and sorts", async () => {
  const self = { hostName: "self", dnsName: "", url: "http://100.64.0.1:7337/" };
  const host = (hostName: string) => ({ ...self, hostName, os: "linux", online: true });
  const fetcher = (url: string) => {
    expect(url).toBe("/v1/hosts");
    return Promise.resolve(Response.json({ self, hosts: [host("z"), host("a")] }));
  };
  expect((await fetchHosts(fetcher)).hosts.map((h: { hostName: string }) => h.hostName)).toEqual(["a", "z"]);
  expect((await fetchHosts(async () => Response.json({ self, hosts: [] }))).hosts).toEqual([]);
  await expect(fetchHosts(async () => new Response(null, { status: 403 }))).rejects.toThrow();
  await expect(fetchHosts(async () => Response.json({}))).rejects.toThrow();
});
