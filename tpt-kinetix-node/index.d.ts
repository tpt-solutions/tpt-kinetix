// Type declarations for index.js. Keep in step with index.js.

/** One track of a probed file. Fields depend on the codec and the container. */
export interface Stream {
  index: number;
  codec: string;
  type: string;
  width?: number;
  height?: number;
  channels?: number;
  sample_rate?: number;
  [key: string]: unknown;
}

/** The result of `probe`. */
export interface ProbeResult {
  format: string;
  streams: Stream[];
}

/** Options for `remux`. */
export interface RemuxOptions {
  /** Move the MP4 index to the front of the file. */
  faststart?: boolean;
  /** Fragment length in milliseconds, for fragmented MP4 output. */
  fragmentMs?: number;
}

/** Options for `package`. */
export interface PackageOptions {
  /** Target segment length in seconds. Defaults to 6. */
  segmentSeconds?: number;
  /** CENC key ID, 32 hex digits. Needs `key`. */
  keyId?: string;
  /** CENC content key, 32 hex digits. Needs `keyId`. */
  key?: string;
  /** CENC scheme. Defaults to `cenc`. */
  scheme?: 'cenc' | 'cbcs';
}

/** The result of `package`. */
export interface PackageResult {
  tracks: number;
  segments: number;
  files: number;
  bytes: number;
}

/** Version of the native engine. */
export const version: string;

/** Lists the tracks of a WebM, Matroska, IVF or MP4 file. */
export function probe(path: string): ProbeResult;

/** Writes a copy of `input` with the MP4 index moved to the front. */
export function faststart(input: string, output: string): void;

/** Copies the streams of `input` into `output` without decoding. */
export function remux(input: string, output: string, options?: RemuxOptions): void;

/** Writes an HLS + DASH package of `input` into `outDir`. */
export function pkg(input: string, outDir: string, options?: PackageOptions): Promise<PackageResult>;
export { pkg as package };
