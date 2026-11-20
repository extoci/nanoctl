export const workloads = ["marker", "text", "drag", "gradient"] as const;
export function workloadName(value = "marker") {
  if (!workloads.includes(value as (typeof workloads)[number]))
    throw new Error(`Unknown workload: ${value}`);
  return value as (typeof workloads)[number];
}

export function drawWorkload(ctx: CanvasRenderingContext2D, frame: number, workload: string) {
  const code = [
    "export async function captureFrame(display: Display) {",
    "  const damage = await display.nextDamage();",
    "  if (!damage.changed) return previousFrame;",
    "  const pixels = capture.read(damage.bounds);",
    "  const timestamp = clock.monotonic();",
    "  return encoder.encode({ pixels, timestamp });",
    "}",
    "// Quality matters: Il1 O0 {} [] () => === !==",
    "const label = `Frame ${sequence}: café / 日本語`;",
    "const colors = [0x102030, 0xabcdef, 0x808080];",
  ];
  const panel = (x: number, y: number, width: number, height: number, scroll: number) => {
    ctx.fillStyle = "#182333";
    ctx.fillRect(x, y, width, height);
    ctx.save();
    ctx.beginPath();
    ctx.rect(x, y, width, height);
    ctx.clip();
    ctx.font = "16px monospace";
    for (let line = 0; line < 30; line++) {
      const row = line * 23 - scroll;
      ctx.fillStyle = ["#d4d4d4", "#9cdcfe", "#ce9178", "#c586c0"][line % 4];
      ctx.fillText(
        `${String(line + 1).padStart(2)}  ${code[line % code.length]}`,
        x + 12,
        y + 24 + row,
      );
    }
    ctx.restore();
  };
  if (workload === "text") {
    panel(0, 220, 1280, 500, (frame * 2) % 230);
    ctx.fillStyle = "#264f78";
    ctx.fillRect(70, 330, 440 + (frame % 80), 22);
    ctx.fillStyle = "white";
    ctx.font = "16px monospace";
    ctx.fillText(`const edited = "${"benchmark input ".repeat(6).slice(0, frame % 90)}";`, 80, 347);
  } else if (workload === "drag") {
    for (let y = 220; y < 720; y += 40)
      for (let x = 0; x < 1280; x += 40) {
        ctx.fillStyle = (x + y) % 80 ? "#526477" : "#718499";
        ctx.fillRect(x, y, 40, 40);
      }
    const x = 100 + Math.round(95 * Math.sin(frame / 35)),
      y = 240 + Math.round(30 * Math.cos(frame / 43));
    panel(x, y, 1050, 400, 0);
    ctx.fillStyle = "#287bc1";
    ctx.fillRect(x, y, 1050, 26);
    ctx.fillStyle = "white";
    ctx.font = "16px sans-serif";
    ctx.fillText("Editor — dragging a large window", x + 12, y + 19);
  } else if (workload === "gradient") {
    const gradient = ctx.createLinearGradient(0, 220, 1280, 720);
    for (let i = 0; i <= 8; i++)
      gradient.addColorStop(i / 8, `hsl(${(frame * 2 + i * 45) % 360} 85% 55%)`);
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 220, 1280, 500);
    for (let i = 0; i < 12; i++) {
      ctx.fillStyle = `hsla(${(frame + i * 30) % 360} 90% 30% / 0.45)`;
      ctx.beginPath();
      ctx.arc(
        ((frame * (i + 2) + i * 123) % 1400) - 60,
        420 + Math.sin(frame / 25 + i) * 120,
        90,
        0,
        Math.PI * 2,
      );
      ctx.fill();
    }
    panel(20, 230, 650, 180, 0);
  }
}
