// One real left click in the broadway page, through headless Chrome's DevTools protocol.
//   node tools/click.mjs PORT POSFILE     (waits up to 30 s for POSFILE to hold "x y")
import { readFileSync, existsSync } from "node:fs";
const [port, posfile] = process.argv.slice(2);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
for (let i = 0; i < 300 && !existsSync(posfile); i++) await sleep(100);
if (!existsSync(posfile)) {
  console.log("nothing to click: the app didn't find the text");
  process.exit(1);
}
const [x, y] = readFileSync(posfile, "utf8").trim().split(/\s+/).map(Number);
const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = pages.find((p) => p.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r));
let id = 0;
const send = (method, params) =>
  new Promise((r) => {
    const n = ++id;
    ws.addEventListener("message", function h(e) {
      if (JSON.parse(e.data).id === n) {
        ws.removeEventListener("message", h);
        r();
      }
    });
    ws.send(JSON.stringify({ id: n, method, params }));
  });
await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
await sleep(150);
await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
await sleep(80);
await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
ws.close();
