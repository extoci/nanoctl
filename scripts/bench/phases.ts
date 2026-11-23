export function trialPlan(
  quality: boolean,
  idleInput: boolean,
  qualityInput: boolean,
  requested?: string,
): { trials: number; duration: number } {
  if (quality && idleInput) throw new Error("Quality and idle-input modes are separate trials");
  if (qualityInput && !quality) throw new Error("Quality input trials require BENCH_QUALITY=1");
  const trials = quality && !qualityInput ? 0 : Number(requested ?? 120);
  if (!Number.isInteger(trials) || trials > 250 || (trials < 100 && (!quality || qualityInput)))
    throw new Error("Invalid trials (100..250)");
  return {
    trials,
    duration: trials === 0 ? 20 : Math.ceil(2 + trials * (idleInput ? 0.85 : 0.45) + 3),
  };
}
