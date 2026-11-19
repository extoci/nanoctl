import { useEffect, useRef, useState } from "react";
import type { HostsResponse } from "@nanoctl/protocol";
import { fetchHosts } from "./hosts";

export function HostPicker({ name }: { name: string }) {
  const [hosts, setHosts] = useState<HostsResponse | null>(null);
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    let active = true;
    const refresh = () =>
      fetchHosts()
        .then((data) => {
          if (active) setHosts(data);
        })
        .catch(() => {
          if (active) {
            setHosts(null);
            setOpen(false);
          }
        });
    void refresh();
    const timer = setInterval(() => void refresh(), 30_000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);
  if (!hosts?.hosts.length) return <strong>{name}</strong>;
  return (
    <div
      className="host-picker"
      ref={root}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          setOpen(false);
          trigger.current?.focus();
        }
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          setOpen(true);
          requestAnimationFrame(() => {
            const buttons = Array.from(
              root.current?.querySelectorAll<HTMLButtonElement>(".host-options button") ?? [],
            );
            const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
            buttons[
              (index + (event.key === "ArrowDown" ? 1 : buttons.length - 1) + buttons.length) %
                buttons.length
            ]?.focus();
          });
        }
      }}
    >
      <button
        ref={trigger}
        className="host-trigger"
        aria-label="Choose host"
        aria-expanded={open}
        aria-controls="host-options"
        onClick={() => setOpen(!open)}
      >
        {name} <small>({hosts.hosts.length + 1}) ▾</small>
      </button>
      {open && (
        <div id="host-options" className="host-options" role="group" aria-label="Tailnet hosts">
          <button aria-current="page" onClick={() => setOpen(false)}>
            <span className="secure-dot" />
            {hosts.self.hostName}
            <small>Current host</small>
          </button>
          {hosts.hosts.map((host) => (
            <button key={host.url} onClick={() => window.location.assign(host.url)}>
              <span
                className={host.online ? "secure-dot" : "offline-dot"}
                aria-label={host.online ? "Online" : "Offline"}
              />
              {host.hostName}
              <small>{host.os}</small>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
