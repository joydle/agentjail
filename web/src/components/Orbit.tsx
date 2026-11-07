/**
 * Ambient background: drifting color orbs + grid + vignette.
 * Mounted once behind the shell to give the whole app a living atmosphere.
 */
export function Orbit() {
  return (
    <div aria-hidden className="fixed inset-0 -z-10 overflow-hidden bg-ink-950">
      <div
        className="orb"
        style={{
          top: "-10%",
          left: "-10%",
          width: "55vw",
          height: "55vw",
          background: "radial-gradient(circle, color-mix(in oklab, var(--color-phantom) 40%, transparent), transparent 60%)",
          animation: "drift-a 22s ease-in-out infinite",
        }}
      />
      <div
        className="orb"
        style={{
          top: "30%",
          right: "-15%",
          width: "50vw",
          height: "50vw",
          background: "radial-gradient(circle, color-mix(in oklab, var(--color-iris) 35%, transparent), transparent 60%)",
          animation: "drift-b 28s ease-in-out infinite",
        }}
      />
      <div
        className="orb"
        style={{
          bottom: "-20%",
          left: "20%",
          width: "45vw",
          height: "45vw",
          background: "radial-gradient(circle, color-mix(in oklab, var(--color-flare) 25%, transparent), transparent 60%)",
          animation: "drift-c 34s ease-in-out infinite",
        }}
      />
      <div className="absolute inset-0 grid-bg opacity-40" />
      <div
        className="absolute inset-0"
        style={{
          background: "radial-gradient(ellipse at 50% 120%, transparent 40%, var(--color-ink-950) 75%)",
        }}
      />
    </div>
  );
}
