// File: ballista/sdks/node/src/bundle.ts
import { readFileSync, statSync } from "node:fs";

import { ConfigurationError } from "./errors.js";
import { checkLabels } from "./models.js";
import { Plan } from "./plan.js";

export const MAX_FILES = 256;
export const MAX_FILE_BYTES = 16 * 1024 * 1024;
export const MAX_TOTAL_BYTES = 64 * 1024 * 1024;
export const MAX_PLAN_BYTES = 1024 * 1024;
const SEGMENT = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}$/;
const RESERVED = new Set(["plan.toml", "bundle.json"]);

/** A structured plan, a raw plan document, or plan file text. */
export type PlanInput = Plan | Record<string, unknown> | string;

/** Mirrors the server rule: relative, at most 8 segments, no hidden or parent segments. */
export function validBundlePath(rel: string): boolean {
  if (!rel || rel.length > 200 || rel.startsWith("/")) return false;
  const parts = rel.split("/");
  return parts.length <= 8 && parts.every((p) => SEGMENT.test(p)) && parts[0] !== "inline";
}

/** The `plan` field sent to the server. */
export function planPayload(plan: PlanInput): Record<string, unknown> | string {
  if (plan instanceof Plan) return plan.toDocument();
  if (typeof plan === "string") {
    if (!plan.trim() || Buffer.byteLength(plan) > MAX_PLAN_BYTES) throw new ConfigurationError("plan text must be non-empty and at most 1 MiB");
    return plan;
  }
  if (typeof plan === "object" && plan !== null && !Array.isArray(plan)) return { ...plan };
  throw new ConfigurationError("plan must be a Plan, a document object or plan text");
}

export interface BundlePayload {
  plan: Record<string, unknown> | string;
  files: Record<string, string>;
  labels: Record<string, string>;
}

/**
 * A plan and optional assets (descriptor sets, certificates, upload files) sent as content.
 * Inline scripts (`source`) and inline feeder `rows` need no files.
 */
export class Bundle {
  readonly plan: Record<string, unknown> | string;
  readonly labels: Record<string, string>;
  readonly #files = new Map<string, Buffer>();

  constructor(plan: PlanInput, options: { labels?: Record<string, string> } = {}) {
    this.plan = planPayload(plan);
    this.labels = checkLabels(options.labels ?? {});
  }

  addBytes(rel: string, data: Uint8Array): this {
    if (!validBundlePath(rel) || RESERVED.has(rel)) throw new ConfigurationError(`invalid bundle path '${rel}'`);
    if (data.byteLength > MAX_FILE_BYTES) throw new ConfigurationError(`'${rel}' exceeds 16 MiB`);
    if (!this.#files.has(rel) && this.#files.size >= MAX_FILES) throw new ConfigurationError(`a bundle holds at most ${MAX_FILES} files`);
    let total = data.byteLength;
    for (const [k, v] of this.#files) if (k !== rel) total += v.byteLength;
    if (total > MAX_TOTAL_BYTES) throw new ConfigurationError("bundle exceeds 64 MiB");
    this.#files.set(rel, Buffer.from(data));
    return this;
  }

  addText(rel: string, text: string): this {
    return this.addBytes(rel, Buffer.from(text, "utf8"));
  }

  /** Adds the content of a local file under the bundle path `rel`. */
  addPath(rel: string, path: string): this {
    const st = statSync(path);
    if (!st.isFile()) throw new ConfigurationError(`'${rel}': ${path} is not a regular file`);
    if (st.size > MAX_FILE_BYTES) throw new ConfigurationError(`'${rel}' exceeds 16 MiB`);
    return this.addBytes(rel, readFileSync(path));
  }

  /** Relative path to size in bytes. */
  get files(): Record<string, number> {
    return Object.fromEntries([...this.#files.entries()].sort(([a], [b]) => (a < b ? -1 : 1)).map(([k, v]) => [k, v.byteLength]));
  }

  toPayload(): BundlePayload {
    return {
      plan: this.plan,
      files: Object.fromEntries([...this.#files.entries()].map(([k, v]) => [k, v.toString("base64")])),
      labels: { ...this.labels },
    };
  }

  toString(): string {
    return `Bundle(plan=${typeof this.plan === "string" ? "text" : "document"}, files=${this.#files.size})`;
  }
}
