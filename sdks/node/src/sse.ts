// File: ballista/sdks/node/src/sse.ts
export interface SseMessage {
  event: string;
  data: string;
  id?: string;
}

/** Incremental `text/event-stream` parser over decoded text chunks. */
export async function* parseSse(chunks: AsyncIterable<string>): AsyncGenerator<SseMessage> {
  let buffer = "";
  let event = "message";
  let data: string[] = [];
  let id: string | undefined;
  for await (const chunk of chunks) {
    buffer += chunk;
    if (buffer.length > 16 * 1024 * 1024) throw new Error("event stream line too long");
    let nl: number;
    while ((nl = buffer.search(/\r\n|\r|\n/)) >= 0) {
      const line = buffer.slice(0, nl);
      const sepLen = buffer.startsWith("\r\n", nl) ? 2 : 1;
      if (sepLen === 1 && buffer[nl] === "\r" && nl + 1 === buffer.length) break;
      buffer = buffer.slice(nl + sepLen);
      if (line === "") {
        if (data.length) yield { event, data: data.join("\n"), ...(id !== undefined ? { id } : {}) };
        event = "message";
        data = [];
        continue;
      }
      if (line.startsWith(":")) continue;
      const colon = line.indexOf(":");
      const field = colon < 0 ? line : line.slice(0, colon);
      let value = colon < 0 ? "" : line.slice(colon + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "event") event = value;
      else if (field === "data") data.push(value);
      else if (field === "id" && !value.includes("\0")) id = value;
    }
  }
}
