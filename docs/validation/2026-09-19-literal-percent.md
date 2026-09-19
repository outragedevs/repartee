# Literal percent rendering

Chat payloads are data, while theme templates and local preformatted notices
contain percent control sequences. The renderers now preserve literal percent
characters in incoming/own messages, actions, notices and topics, including
`%`, `100%`, `%N`, `%%`, hex-looking sequences and percent-encoded URLs.
Standard mIRC control bytes still apply bold and colors.

Native templates escape literal parameters at the rendering boundary. Mention
aggregation escapes its dynamic fields once and retains its trusted formatting.
Structured events retain their event key across history conversion; templates
are used only when their parameters exist. The fallback renders those event
bodies as IRC text. Local preformatted events without an event key retain their
existing formatting behavior. Web snapshots escape structured event bodies once
for the formatted-event renderer; ordinary chat uses the literal-data path.

Validation:

- Native tests cover both shipped themes, own/received messages, ACTION, NOTICE,
  IRC bold, translated originals, mention aggregation and padded parameters.
- A history conversion test checks live/native history output and equality of
  live, in-memory history and direct database web snapshots; local formatted
  events remain formatted.
- Host-side web tests cover literal text and IRC styles.
- Headless WebKit loaded the actual rebuilt WASM with a synthetic WebSocket
  fixture. DOM assertions verified own/received chat, ACTION, NOTICE, topic,
  translated suffix, a mention row, a formatted event, IRC bold and the exact
  `https://example.org/a%20b` link target. No browser errors were reported.
- `make clippy`, `make test` (2309 native, 139 web), `make wasm` and `make build`
  passed. Cargo retains the existing proc-macro-error2 future-compatibility notice.

Browser harness and capture are temporary local artifacts:
`/tmp/repartee-browser-qa/percent.cjs`, `/tmp/repartee-percent-browser3.log`,
and `/tmp/repartee-percent-web.png`. No live IRC traffic or user credentials
were used.
