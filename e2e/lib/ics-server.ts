import { createServer, Server } from "node:http";
import type { AddressInfo } from "node:net";

// A throwaway static file server for the Google Calendar import suite (front
// epic F11, issue #52).
//
// The epic's backend fetches a private ICS feed URL over HTTP when someone
// triggers an import. Pointing that at Google in CI would mean a real account,
// real credentials and network egress, so the suite serves its own `.ics`
// fixtures instead: `validate_feed_url` deliberately accepts `http://` alongside
// `https://` precisely so a loopback feed can be used without TLS (see its doc
// comment in apps/api/src/google_calendar/imports.rs).
//
// **apps/api is the client here, not the browser.** The URL handed to the app
// has to be reachable from the apps/api process, so the server binds 0.0.0.0 and
// the advertised host is configurable: `127.0.0.1` when everything runs on one
// machine (ci.yml's `e2e` job), or the Playwright container's hostname when the
// stack runs on a docker network (see e2e/README.md).

export interface IcsFixture {
  /** Absolute URL of `path`, as apps/api will fetch it. */
  url(path: string): string;
  /** Serve `body` at `path` (replaces whatever was there). */
  serve(path: string, body: string, contentType?: string): void;
  /** Stop serving `path` — subsequent fetches get a 404. */
  remove(path: string): void;
  close(): Promise<void>;
}

interface Entry {
  body: string;
  contentType: string;
}

export async function startIcsFixtureServer(): Promise<IcsFixture> {
  const entries = new Map<string, Entry>();

  const server: Server = createServer((req, res) => {
    const path = (req.url ?? "/").split("?")[0];
    const entry = entries.get(path);
    if (!entry) {
      res.writeHead(404, { "content-type": "text/plain" });
      res.end("not found");
      return;
    }
    res.writeHead(200, { "content-type": entry.contentType });
    res.end(entry.body);
  });

  await new Promise<void>((resolve) => server.listen(0, "0.0.0.0", resolve));
  const port = (server.address() as AddressInfo).port;
  // Whatever apps/api can reach us at. Defaults to loopback, which is what CI
  // (both processes on the runner) uses.
  const host = process.env.ICS_FIXTURE_HOST ?? "127.0.0.1";

  return {
    url: (path: string) => `http://${host}:${port}${path}`,
    serve: (path: string, body: string, contentType = "text/calendar") => {
      entries.set(path, { body, contentType });
    },
    remove: (path: string) => {
      entries.delete(path);
    },
    close: () =>
      new Promise<void>((resolve, reject) =>
        server.close((err) => (err ? reject(err) : resolve())),
      ),
  };
}

/** `YYYYMMDD` for a day of the month currently on screen in `/agenda`. */
export function icsDayThisMonth(day: number): string {
  const now = new Date();
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  return `${y}${m}${String(day).padStart(2, "0")}`;
}

export interface IcsEvent {
  uid: string;
  summary: string;
  /** `YYYYMMDD`, see `icsDayThisMonth`. */
  day: string;
  /** `HHMMSS` UTC, defaults to a mid-morning slot. */
  startTime?: string;
  endTime?: string;
  /** `YYYYMMDDTHHMMSSZ`. Bumping it is what makes a re-import an *update*
   *  rather than a skip — apps/api compares it to the stored value. */
  lastModified: string;
  location?: string;
  description?: string;
}

/** A minimal Google-shaped VCALENDAR wrapping the given events. */
export function icsFeed(events: IcsEvent[]): string {
  const body = events
    .map((e) =>
      [
        "BEGIN:VEVENT",
        `UID:${e.uid}`,
        "DTSTAMP:20260101T090000Z",
        `DTSTART:${e.day}T${e.startTime ?? "100000"}Z`,
        `DTEND:${e.day}T${e.endTime ?? "110000"}Z`,
        `SUMMARY:${e.summary}`,
        ...(e.description ? [`DESCRIPTION:${e.description}`] : []),
        ...(e.location ? [`LOCATION:${e.location}`] : []),
        `LAST-MODIFIED:${e.lastModified}`,
        "END:VEVENT",
      ].join("\r\n"),
    )
    .join("\r\n");
  return [
    "BEGIN:VCALENDAR",
    "VERSION:2.0",
    "PRODID:-//Google Inc//Google Calendar 70.9054//EN",
    body,
    "END:VCALENDAR",
    "",
  ].join("\r\n");
}
