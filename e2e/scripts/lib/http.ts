//! A deliberately unforgiving HTTP client.
//!
//! The incident these scripts exist to prevent started with a measurement
//! loop that wrote a 0-byte file for a route that 404'd and carried on as if
//! it had a number. So: every call states the statuses it accepts, anything
//! else throws, and the message carries the method, the URL, the status and
//! the response body. There is no "continue on error" path here on purpose.

// NB : pas de « parameter properties » (`constructor(readonly x: T)`) ni
// d'`enum` dans ce répertoire — Node exécute ces fichiers .ts en effaçant les
// types, sans les transformer, et refuse toute syntaxe qui produirait du code.
export class HttpFailure extends Error {
  readonly method: string;
  readonly url: string;
  readonly status: number;
  readonly body: string;
  readonly expected: number[];

  constructor(method: string, url: string, status: number, body: string, expected: number[]) {
    super(
      `${method} ${url} → HTTP ${status} (attendu : ${expected.join(" ou ")})\n` +
        `corps de la réponse : ${body.slice(0, 1000)}${body.length > 1000 ? " […]" : ""}`,
    );
    this.name = "HttpFailure";
    this.method = method;
    this.url = url;
    this.status = status;
    this.body = body;
    this.expected = expected;
  }
}

export type JsonValue = unknown;

/**
 * A cookie jar + fetch wrapper. `fetch` in Node keeps no cookies, and the
 * session cookie is the whole point here, so the jar is explicit.
 */
export class HttpSession {
  private readonly cookies = new Map<string, string>();
  readonly baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  /** `name=value; name=value` for the `Cookie` header, or `undefined`. */
  cookieHeader(): string | undefined {
    if (this.cookies.size === 0) return undefined;
    return [...this.cookies].map(([k, v]) => `${k}=${v}`).join("; ");
  }

  setCookie(name: string, value: string): void {
    this.cookies.set(name, value);
  }

  private absorbCookies(res: Response): void {
    for (const raw of res.headers.getSetCookie()) {
      const [pair] = raw.split(";");
      const eq = pair.indexOf("=");
      if (eq > 0) this.cookies.set(pair.slice(0, eq).trim(), pair.slice(eq + 1).trim());
    }
  }

  /** Raw request. Throws `HttpFailure` unless the status is in `expect`. */
  async raw(
    method: string,
    path: string,
    opts: { body?: JsonValue; expect?: number[]; headers?: Record<string, string> } = {},
  ): Promise<{ status: number; text: string; headers: Headers }> {
    const expect = opts.expect ?? [200, 201, 204];
    const url = `${this.baseUrl}${path}`;
    const headers: Record<string, string> = { ...opts.headers };
    const cookie = this.cookieHeader();
    if (cookie) headers.Cookie = cookie;
    if (opts.body !== undefined) headers["Content-Type"] = "application/json";

    let res: Response;
    try {
      res = await fetch(url, {
        method,
        headers,
        body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
        redirect: "manual",
      });
    } catch (cause) {
      throw new Error(`${method} ${url} : la requête n'a pas abouti (${String(cause)})`, { cause });
    }

    const text = await res.text();
    this.absorbCookies(res);
    if (!expect.includes(res.status)) {
      throw new HttpFailure(method, url, res.status, text, expect);
    }
    return { status: res.status, text, headers: res.headers };
  }

  /** Request whose body is parsed as JSON (throws if it isn't JSON). */
  async json<T = JsonValue>(
    method: string,
    path: string,
    opts: { body?: JsonValue; expect?: number[] } = {},
  ): Promise<T> {
    const { status, text } = await this.raw(method, path, opts);
    if (status === 204 || text.length === 0) return undefined as T;
    try {
      return JSON.parse(text) as T;
    } catch {
      throw new Error(
        `${method} ${this.baseUrl}${path} a répondu ${status} avec un corps non-JSON :\n${text.slice(0, 500)}`,
      );
    }
  }
}

/** Polls `path` until it answers anything at all, or gives up loudly. */
export async function waitForService(
  baseUrl: string,
  path: string,
  timeoutMs = 60_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastError = "aucune tentative";
  while (Date.now() < deadline) {
    try {
      await fetch(`${baseUrl}${path}`, { redirect: "manual" });
      return;
    } catch (e) {
      lastError = String(e);
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  throw new Error(
    `${baseUrl}${path} injoignable après ${timeoutMs} ms (dernière erreur : ${lastError})`,
  );
}
