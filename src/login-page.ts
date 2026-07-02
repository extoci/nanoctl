export function loginPage(error = ""): string {
  const errorHtml = error
    ? `<p class="error">${error}</p>`
    : `<p class="hint">enter the host pin to open the live session.</p>`;

  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <title>Nanoctl Login</title>
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
  <link href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;700&display=swap" rel="stylesheet" />
  <style>
    :root {
      --bg: #111417;
      --panel: #182027;
      --line: #2e3d49;
      --ink: #f4f7f9;
      --muted: #95a9b8;
      --accent: #c4ff62;
      --error: #ff7d7d;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      display: grid;
      place-items: center;
      background:
        radial-gradient(circle at top, rgba(196, 255, 98, .12), transparent 30%),
        linear-gradient(160deg, #101315 0%, #0b0e10 100%);
      color: var(--ink);
      font-family: "IBM Plex Mono", monospace;
      padding: 1.2rem;
    }
    .panel {
      width: min(100%, 420px);
      border: 1px solid var(--line);
      background: rgba(24, 32, 39, .92);
      box-shadow: 0 20px 60px rgba(0, 0, 0, .35);
      padding: 1.3rem;
    }
    h1 {
      margin: 0 0 .45rem;
      font-size: 1.45rem;
    }
    .hint, .error {
      margin: 0 0 1rem;
      color: var(--muted);
      line-height: 1.6;
    }
    .error { color: var(--error); }
    input, button {
      width: 100%;
      border: 1px solid var(--line);
      background: #0e1317;
      color: var(--ink);
      font: inherit;
      padding: .92rem 1rem;
    }
    input { margin-bottom: .8rem; }
    button {
      background: var(--accent);
      color: #0b0e10;
      font-weight: 700;
      cursor: pointer;
    }
  </style>
</head>
<body>
  <form class="panel" method="post" action="/auth">
    <h1>Nanoctl</h1>
    ${errorHtml}
    <input name="pin" inputmode="numeric" autocomplete="one-time-code" placeholder="6-digit PIN" required />
    <button type="submit">Connect</button>
  </form>
</body>
</html>`;
}
