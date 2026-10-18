## 2026-10-18 · One control-client admission deadline (#289)

- Initial connect and replacement authentication use one checked five-second network deadline,
  including partial writes, reads and the landed six-attempt Busy/Network retry backoff. Hello
  uses the existing 4 KiB handshake bound, independently of large screenshot replies.
- Admission retries never replay the caller's operation. Keep the landed reconnect threshold
  setter, disconnected recovery, server capacity and refusal-worker behavior. Token-file reads
  remain synchronous; check elapsed admission before network work without claiming interruptible
  filesystem I/O. Compose with the landed 4 KiB token-file read bound and token-shape validation
  before connecting, including every replacement attempt.
