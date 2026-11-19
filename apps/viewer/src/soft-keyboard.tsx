import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ControlMessage } from "@nanoctl/protocol";
import {
  inputMessages,
  keyMessages,
  namedKeys,
  modifierKeys,
  StickyModifiers,
  SENTINEL,
  type Modifier,
} from "./lib/soft-keyboard";

const specialKeys = [
  ["Esc", "Escape"],
  ["Tab", "Tab"],
  ["←", "ArrowLeft"],
  ["↑", "ArrowUp"],
  ["↓", "ArrowDown"],
  ["→", "ArrowRight"],
  ["Home", "Home"],
  ["End", "End"],
  ["PgUp", "PageUp"],
  ["PgDn", "PageDown"],
  ["Del", "Delete"],
] as const;

export function SoftKeyboard({
  enabled,
  platform,
  send,
}: {
  enabled: boolean;
  platform?: string;
  send: (message: ControlMessage) => unknown;
}) {
  const field = useRef<HTMLTextAreaElement>(null);
  const sink = useRef(send);
  sink.current = send;
  const [sticky] = useState(() => new StickyModifiers((message) => sink.current(message)));
  const [, refresh] = useState(0);
  const [open, setOpen] = useState(false);
  const [showKeys, setShowKeys] = useState(false);
  const [bottom, setBottom] = useState(0);
  const update = () => refresh((n) => n + 1);

  useEffect(() => {
    const viewport = window.visualViewport;
    const position = () =>
      setBottom(
        viewport ? Math.max(0, window.innerHeight - viewport.height - viewport.offsetTop) : 0,
      );
    viewport?.addEventListener("resize", position);
    viewport?.addEventListener("scroll", position);
    position();
    return () => {
      viewport?.removeEventListener("resize", position);
      viewport?.removeEventListener("scroll", position);
    };
  }, []);

  useEffect(() => {
    const input = field.current;
    if (!input) return;
    input.value = SENTINEL;
    let composing = false;
    let committed: string | null = null;
    let commitTimer: ReturnType<typeof setTimeout> | undefined;
    let handled = false;
    const reset = () => {
      input.value = SENTINEL;
      input.setSelectionRange(1, 1);
    };
    const dispatch = (type: string, data: string | null) => {
      if (!enabled) return;
      sticky.dispatch(inputMessages(type, data, sticky.mask));
      update();
    };
    const before = (event: InputEvent) => {
      handled = false;
      if (composing || event.isComposing) return;
      if (
        committed !== null &&
        (event.inputType === "insertFromComposition" || event.data === committed)
      ) {
        if (event.cancelable) event.preventDefault();
        handled = true;
        committed = null;
        reset();
        return;
      }
      committed = null;
      if (event.cancelable) {
        event.preventDefault();
        dispatch(event.inputType, event.data);
        handled = true;
        reset();
      }
    };
    const onInput = (event: Event) => {
      const e = event as InputEvent;
      if (composing || e.isComposing) return;
      if (
        !handled &&
        !(committed !== null && (e.inputType === "insertFromComposition" || e.data === committed))
      ) {
        dispatch(e.inputType, e.data ?? input.value.replace(SENTINEL, ""));
      }
      handled = false;
      committed = null;
      reset();
    };
    const start = () => {
      composing = true;
      committed = null;
      handled = false;
    };
    const paste = (event: ClipboardEvent) => {
      event.preventDefault();
      dispatch("insertFromPaste", event.clipboardData?.getData("text/plain") ?? "");
      reset();
    };
    const end = (event: CompositionEvent) => {
      composing = false;
      dispatch("insertCompositionText", event.data);
      committed = event.data;
      clearTimeout(commitTimer);
      commitTimer = setTimeout(() => {
        committed = null;
        handled = false;
      }, 0);
      reset();
    };
    const key = (event: KeyboardEvent) => {
      if (event.isComposing || event.keyCode === 229 || composing) return;
      committed = null;
      handled = false;
      if (!namedKeys[event.key]) return;
      event.preventDefault();
      event.stopPropagation();
      if (enabled) sticky.dispatch(keyMessages(event.key, namedKeys[event.key], sticky.mask));
      update();
      reset();
    };
    const release = () => {
      composing = false;
      committed = null;
      sticky.reset();
      update();
    };
    const blur = () => {
      release();
      setOpen(false);
    };
    const hide = () => {
      if (document.visibilityState === "hidden") {
        release();
        input.blur();
      }
    };
    input.addEventListener("beforeinput", before);
    input.addEventListener("input", onInput);
    input.addEventListener("paste", paste);
    input.addEventListener("compositionstart", start);
    input.addEventListener("compositionend", end);
    input.addEventListener("keydown", key);
    input.addEventListener("blur", blur);
    window.addEventListener("blur", blur);
    document.addEventListener("visibilitychange", hide);
    if (!enabled) {
      release();
      input.blur();
      setOpen(false);
    }
    return () => {
      clearTimeout(commitTimer);
      sticky.reset();
      input.removeEventListener("beforeinput", before);
      input.removeEventListener("input", onInput);
      input.removeEventListener("paste", paste);
      input.removeEventListener("compositionstart", start);
      input.removeEventListener("compositionend", end);
      input.removeEventListener("keydown", key);
      input.removeEventListener("blur", blur);
      window.removeEventListener("blur", blur);
      document.removeEventListener("visibilitychange", hide);
    };
  }, [enabled, sticky]);

  return (
    <>
      <button
        disabled={!enabled}
        aria-pressed={open}
        onPointerDown={(e) => e.preventDefault()}
        onClick={() => {
          if (open) field.current?.blur();
          else {
            field.current?.focus({ preventScroll: true });
            setShowKeys(true);
          }
        }}
      >
        Keyboard
      </button>
      <button
        disabled={!enabled}
        aria-pressed={showKeys}
        onPointerDown={(e) => e.preventDefault()}
        onClick={() => setShowKeys(!showKeys)}
      >
        Keys
      </button>
      {createPortal(
        <>
          <textarea
            ref={field}
            className="soft-keyboard-field"
            tabIndex={-1}
            aria-label="Remote keyboard"
            disabled={!enabled}
            autoCapitalize="off"
            autoCorrect="off"
            autoComplete="off"
            spellCheck={false}
            inputMode="text"
            enterKeyHint="send"
            onFocus={() => setOpen(true)}
          />
          {showKeys && enabled && (
            <div
              className="special-keys"
              role="toolbar"
              aria-label="Remote special keys"
              style={{ bottom }}
            >
              {Object.keys(modifierKeys).map((key) => (
                <button
                  key={key}
                  aria-pressed={sticky.state.has(key as Modifier)}
                  title={sticky.state.get(key as Modifier) ?? "Tap to latch; double tap to lock"}
                  onPointerDown={(e) => e.preventDefault()}
                  onClick={() => {
                    sticky.tap(key as Modifier, performance.now());
                    update();
                  }}
                >
                  {key === "Control"
                    ? "Ctrl"
                    : key === "Meta"
                      ? platform === "macos"
                        ? "Cmd"
                        : "Win"
                      : key}
                  {sticky.state.get(key as Modifier) === "locked" ? " ●" : ""}
                </button>
              ))}
              {specialKeys.map(([label, key]) => (
                <button
                  key={key}
                  aria-label={key}
                  onPointerDown={(e) => e.preventDefault()}
                  onClick={() => {
                    sticky.dispatch(keyMessages(key, key, sticky.mask));
                    update();
                  }}
                >
                  {label}
                </button>
              ))}
              <button
                aria-label="Hide special keys"
                onPointerDown={(e) => e.preventDefault()}
                onClick={() => setShowKeys(false)}
              >
                ×
              </button>
            </div>
          )}
        </>,
        document.body,
      )}
    </>
  );
}
