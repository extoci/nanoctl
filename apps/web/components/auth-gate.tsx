"use client";

import { Authenticated, AuthLoading, Unauthenticated } from "convex/react";
import type { ReactNode } from "react";
import { useState } from "react";
import { signIn } from "../lib/shoo";
import { BrandMark } from "./brand-mark";

const INSTALL_UNIX = "curl -fsSL https://extoci.lol/nanoctl/install | sh";
const INSTALL_WINDOWS = "irm https://extoci.lol/nanoctl/install | iex";

export function AuthGate({ children }: { children: ReactNode }) {
  return (
    <>
      <AuthLoading>
        <main className="auth-loading">
          <p className="eyebrow">nanoctl</p>
          <h1>Opening your workspace…</h1>
          <p className="lede">Checking your session and loading devices.</p>
        </main>
      </AuthLoading>
      <Unauthenticated>
        <Landing />
      </Unauthenticated>
      <Authenticated>{children}</Authenticated>
    </>
  );
}

function Landing() {
  return (
    <main className="landing">
      <nav className="landing-nav" aria-label="Primary">
        <BrandMark />
        <span className="landing-nav-meta">secure remote access</span>
      </nav>

      <section className="landing-hero">
        <div className="landing-hero-copy">
          <p className="eyebrow">nanoctl / secure remote access</p>
          <h1>Your computers, one quiet click away.</h1>
          <p className="lede">
            Direct WebRTC video and control, with encrypted relay fallback when networks get
            difficult.
          </p>
          <div className="landing-cta">
            <button className="primary" type="button" onClick={() => void signIn()}>
              Continue with Shoo
            </button>
            <p className="landing-cta-note">Owner-only access. No agent UI required.</p>
          </div>
        </div>

        <div className="landing-preview" aria-hidden="true">
          <div className="preview-chrome">
            <div className="preview-dots">
              <span />
              <span />
              <span />
            </div>
            <span className="preview-chrome-label">devices · live</span>
          </div>
          <div className="preview-body">
            <div className="preview-device">
              <div>
                <strong>Studio workstation</strong>
                <span>windows / x64 · agent 1.0.17</span>
              </div>
              <span className="preview-status">online</span>
            </div>
            <div className="preview-device">
              <div>
                <strong>Travel Mac</strong>
                <span>macos / arm64 · last seen 2h ago</span>
              </div>
              <span className="preview-status offline">offline</span>
            </div>
            <div className="preview-metrics">
              <div className="preview-metric">
                <small>Route</small>
                <strong>direct</strong>
              </div>
              <div className="preview-metric">
                <small>Latency</small>
                <strong>18 ms</strong>
              </div>
              <div className="preview-metric">
                <small>Stream</small>
                <strong>1080p60</strong>
              </div>
            </div>
          </div>
        </div>
      </section>

      <section className="landing-features" aria-label="Product highlights">
        <article className="feature-card">
          <div className="feature-icon">↔</div>
          <h2>Peer-to-peer first</h2>
          <p>Encrypted WebRTC media goes host to browser. TURN is only the fallback path.</p>
        </article>
        <article className="feature-card">
          <div className="feature-icon">⌘</div>
          <h2>Headless host agent</h2>
          <p>Install a quiet OS service—no tray chrome, no always-on desktop window.</p>
        </article>
        <article className="feature-card">
          <div className="feature-icon">◎</div>
          <h2>Owner authorized</h2>
          <p>One owner per device, one active controller, and a fail-closed session model.</p>
        </article>
        <article className="feature-card">
          <div className="feature-icon">◆</div>
          <h2>Windows, macOS, Linux</h2>
          <p>Native capture and input on the platforms you already run at home or work.</p>
        </article>
      </section>

      <section className="landing-install" aria-label="Install">
        <div>
          <p className="eyebrow">Host setup</p>
          <h2>Pair a computer in one command</h2>
          <p>
            Create a setup code after sign-in, run the installer on the host, and the device appears
            online when ready.
          </p>
        </div>
        <div className="install-commands">
          <CopyableCommand label="Unix" command={INSTALL_UNIX} />
          <CopyableCommand label="Windows" command={INSTALL_WINDOWS} />
        </div>
      </section>

      <footer className="landing-footer">
        Media and input travel over DTLS-SRTP/SCTP. The control plane never sees your desktop
        content.
      </footer>
    </main>
  );
}

function CopyableCommand({ label, command }: { label: string; command: string }) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1_500);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="install-command">
      <code title={`${label}: ${command}`}>{command}</code>
      <button className="ghost" type="button" onClick={() => void copy()}>
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}
