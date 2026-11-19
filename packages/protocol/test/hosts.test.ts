import { expect, test } from "bun:test";
import { assertHostsResponse } from "../src";

test("hosts response validates shape and safe URLs", () => {
  const self = { hostName: "local", dnsName: "local.ts.net", url: "https://local.ts.net:7337/" };
  expect(() => assertHostsResponse({ self, hosts: [] })).not.toThrow();
  expect(() =>
    assertHostsResponse({ self, hosts: [{ ...self, os: "linux", online: true }] }),
  ).not.toThrow();
  for (const url of [
    "javascript:alert(1)",
    "file:///etc/passwd",
    "relative",
    "https://user:pass@host/",
  ]) {
    expect(() => assertHostsResponse({ self: { ...self, url }, hosts: [] })).toThrow();
  }
  expect(() => assertHostsResponse({ self, hosts: [{}] })).toThrow();
  expect(() => assertHostsResponse(null)).toThrow();
});
