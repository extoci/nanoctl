// Classic little-endian tcpdump pcap from macOS lo0 (-y NULL), truncated to the RTP header.
export function rtpMarkerArrivals(pcap: ArrayBuffer, ssrc: number): Map<number, number> {
  const view = new DataView(pcap);
  if (
    view.byteLength < 24 ||
    view.getUint32(0, true) !== 0xa1b2c3d4 ||
    view.getUint32(20, true) !== 0
  ) {
    throw new Error("Expected a macOS loopback tcpdump capture (pcap, DLT_NULL)");
  }
  const arrivals = new Map<number, number>();
  for (let record = 24; record + 16 <= view.byteLength;) {
    const packet = record + 16;
    const length = view.getUint32(record + 8, true);
    const next = packet + length;
    if (next > view.byteLength) break;
    if (length >= 44) {
      const ip = packet + 4;
      const version = view.getUint8(ip) >> 4;
      const rtp = ip + (version === 6 ? 40 : (view.getUint8(ip) & 15) * 4) + 8;
      if (
        (version === 4 || version === 6) &&
        rtp + 12 <= next &&
        (view.getUint8(rtp) & 0xc0) === 0x80 &&
        (view.getUint8(rtp + 1) & 0x80) !== 0 &&
        view.getUint32(rtp + 8) === ssrc
      ) {
        const timestamp = view.getUint32(rtp + 4);
        const receivedMs =
          view.getUint32(record, true) * 1000 + view.getUint32(record + 4, true) / 1000;
        if (!arrivals.has(timestamp)) arrivals.set(timestamp, receivedMs);
      }
    }
    record = next;
  }
  return arrivals;
}
