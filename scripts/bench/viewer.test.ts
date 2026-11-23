import { expect, test } from "bun:test";
import { setupClicks } from "./viewer";

test("viewer setup is empty by default and preserves ordered selectors", () => {
  expect(setupClicks(undefined)).toEqual([]);
  expect(setupClicks("[]")).toEqual([]);
  expect(setupClicks('["#toolbar-toggle", "[data-action=control]"]')).toEqual([
    "#toolbar-toggle",
    "[data-action=control]",
  ]);
});
test("viewer setup rejects malformed or non-selector values", () => {
  for (const text of ["bad json", "null", "{}", '["ok", 4]', '[""]', '["  "]'])
    expect(() => setupClicks(text)).toThrow();
});
