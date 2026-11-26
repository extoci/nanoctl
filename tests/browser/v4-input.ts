import { parseInputMessage, type InputMessage } from "../../packages/protocol/src/index";

export type InjectedWireMessage = { channel?: string; text?: string; binary?: number[] };

export function injectDataChannelSpy(): void {
  const send = RTCDataChannel.prototype.send;
  (window as unknown as { injected: InjectedWireMessage[] }).injected = [];
  RTCDataChannel.prototype.send = function (data: unknown) {
    const injected = (window as unknown as { injected: InjectedWireMessage[] }).injected;
    if (data instanceof ArrayBuffer) {
      injected.push({ channel: this.label, binary: Array.from(new Uint8Array(data)) });
    } else if (ArrayBuffer.isView(data)) {
      injected.push({
        channel: this.label,
        binary: Array.from(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)),
      });
    } else {
      injected.push({ channel: this.label, text: String(data) });
    }
    return Reflect.apply(send, this, [data]);
  };
}

export function injectedWire(page: {
  evaluate<T>(fn: () => T): Promise<T>;
}): Promise<InjectedWireMessage[]> {
  return page.evaluate(() => (window as unknown as { injected: InjectedWireMessage[] }).injected);
}

export function v4InputMessages(messages: InjectedWireMessage[]): InputMessage[] {
  return messages
    .filter(
      (message) =>
        message.binary !== undefined &&
        (message.channel === "nanoctl.control.v4" || message.channel === "nanoctl.motion.v4"),
    )
    .map((message) => parseInputMessage(new Uint8Array(message.binary!)));
}
