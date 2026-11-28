/** Types of perfratio_region.js (protocol 1, SDK 1.0.0). */
export const PROTOCOL: 1;
export const SDK_VERSION: string;
export const SDK_ID: string;
export const DEFAULT_REGION: 'default';
/** True when perfratio asked for region events. */
export function enabled(): boolean;
export function start(name?: string): void;
export function stop(name?: string): void;
/** Run `fn` inside the region; the region stops also when `fn` throws. */
export function region<T>(name: string, fn: () => T): T;
export function region<T>(fn: () => T): T;
/** Write the buffered events now. Done automatically at exit. */
export function flush(): void;
/** Persistent mode (`perfratio --persistent`): call `fn` once per requested iteration. */
export function serve(
  fn: () => void | Promise<void>,
  input?: NodeJS.ReadableStream,
  output?: NodeJS.WritableStream,
): Promise<void>;
