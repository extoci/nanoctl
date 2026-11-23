export function setupClicks(text: string | undefined): string[] {
  const selectors: unknown = JSON.parse(text ?? "[]");
  if (
    !Array.isArray(selectors) ||
    !selectors.every((value) => typeof value === "string" && value.trim().length > 0)
  )
    throw new Error("VIEWER_SETUP_CLICKS must be a JSON array of nonempty CSS selectors");
  return selectors;
}
