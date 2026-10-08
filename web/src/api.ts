/**
 * Fetch wrapper for the /api JSON surface (ARCHITECTURE.md §7).
 * Adds `X-TCR: 1` on every non-GET (CSRF contract), `Idempotency-Key` when
 * given, and parses `{"error":{"code","message","details"}}` into ApiError.
 */

export class ApiError extends Error {
  /** HTTP status; 0 = the request never reached the server (network failure). */
  readonly status: number;
  /** Snake_case code from the error envelope, e.g. "version_conflict". */
  readonly code: string;
  /** Opaque server-provided details object, e.g. `{current}` on conflicts. */
  readonly details: unknown;

  constructor(status: number, code: string, message: string, details?: unknown) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

export interface ApiOptions {
  idempotencyKey?: string;
  signal?: AbortSignal;
}

async function parseError(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let message = res.statusText || `Request failed (${res.status})`;
  let details: unknown;
  try {
    const body = (await res.json()) as { error?: { code?: string; message?: string; details?: unknown } };
    if (body?.error) {
      code = body.error.code ?? code;
      message = body.error.message ?? message;
      details = body.error.details;
    }
  } catch {
    // Non-JSON error body (proxy down, HTML error page) — keep defaults.
  }
  return new ApiError(res.status, code, message, details);
}

function mutationHeaders(opts?: ApiOptions): Record<string, string> {
  const headers: Record<string, string> = { 'X-TCR': '1' };
  if (opts?.idempotencyKey) headers['Idempotency-Key'] = opts.idempotencyKey;
  return headers;
}

async function request<T>(method: string, path: string, init: RequestInit, opts?: ApiOptions): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`/api${path}`, {
      method,
      credentials: 'same-origin',
      signal: opts?.signal,
      ...init,
    });
  } catch (err) {
    if (err instanceof DOMException && err.name === 'AbortError') throw err;
    throw new ApiError(0, 'network', 'Could not reach the server.');
  }
  if (!res.ok) throw await parseError(res);
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

/** JSON request. `body` is serialised when defined; GET never sends a body. */
export function api<T>(method: string, path: string, body?: unknown, opts?: ApiOptions): Promise<T> {
  const headers: Record<string, string> = method === 'GET' ? {} : mutationHeaders(opts);
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  return request<T>(method, path, {
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }, opts);
}

/** Multipart upload: sends FormData as-is (browser sets the boundary). */
export function upload<T>(path: string, form: FormData, opts?: ApiOptions): Promise<T> {
  return request<T>('POST', path, { headers: mutationHeaders(opts), body: form }, opts);
}

/** URL for a browser-driven download (href/download attribute); the session cookie travels with it. */
export function downloadUrl(path: string): string {
  return `/api${path}`;
}

/** Fresh idempotency key for register/close/finalise/queue-dispatch/schedule calls. */
export function newKey(): string {
  return crypto.randomUUID();
}
