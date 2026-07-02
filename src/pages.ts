export { loginPage } from "./login-page";
export { watchPage } from "./watch-page";

export function html(content: string): Response {
  return new Response(content, {
    status: 200,
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}
