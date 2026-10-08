// The Open Food Facts stand-in of the e2e stack (#402):
// `node scripts/off-stub.ts`, listening on OFF_STUB_PORT (default 8090).
// The job `e2e` of .github/workflows/ci.yml starts it before apps/api and
// sets `OPENFOODFACTS_BASE_URL` to it. Answers: `lib/off-stub-core.ts`.

import { createServer } from "node:http";

import { offStubAnswer } from "./lib/off-stub-core.ts";

const port = Number(process.env.OFF_STUB_PORT ?? "8090");

createServer((request, response) => {
  const answer = offStubAnswer(request.url ?? "/");
  response.writeHead(answer.status, { "content-type": "application/json" });
  response.end(JSON.stringify(answer.body));
}).listen(port, "0.0.0.0", () => {
  console.log(`off-stub listening on ${port}`);
});
