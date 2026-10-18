## 2026-10-17 · Trusted headless control stdio (#259)

- The stdio follow-up to #115 lives in `nodecraft-cli control`, against an empty headless session.
  Only this explicit trusted-parent entrypoint starts authorized; TCP still constructs a
  token-required connection. Both reuse the same bounded framing, parser and command policy.
- `hello` remains optional version negotiation on stdio. Malformed, oversized and invalid UTF-8
  lines recover at the next line; EOF processes a final fragment. Output failure terminates with
  a process error. No keychain service, socket, token file or desktop screenshot is introduced.
