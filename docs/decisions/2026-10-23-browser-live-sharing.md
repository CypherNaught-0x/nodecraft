## 2026-10-23 · Live sharing in the browser (#699)

The browser build could not share live: the sharing module needs a socket and a thread, and the
browser has neither. The wire protocol and the relay are unchanged.

**The connection is the page's `WebSocket`.** `sharing/browser.rs` has the same API as the
desktop's `Connection` (`open`, `send(n, bytes)`, `events()`), so the rest of sharing is shared.
Frames come in on the page's queue between the app's frames and wait in an inbox until the app
takes them, at most one frame's worth of bytes per app frame.

- It reconnects by itself as the desktop does: 1 s, doubling to 30 s, timed by `setTimeout`.
  Each connection has a number, and frames sent for an earlier one are dropped.
- The browser answers the relay's pings and hides them, so it can't detect a silent connection
  the way the desktop does. A connection that dies quietly is noticed only when the browser
  closes it.
- It can't stop reading, so it can't push back on a busy relay. A relay that sends more than
  four frames' worth before the app takes them is dropped and connected to again, so a flood
  can't fill memory.
- The socket's handlers hold only a weak reference to the connection's state. A socket that is
  given up is closed and detached at once, and dropped at the next try, once its handlers can no
  longer be running.

**No thread: the worker's work runs in the frame.** In the browser, `Worker` keeps the
`Collab` and what the window asked for. Each `reports()` call:

1. makes the `Collab` the first time it runs;
2. takes in what came on the connection;
3. does up to one batch of the queued asks;
4. asks for another frame if any are left.

It is the same work the desktop thread does (`work`), so a busy room or a large catch-up is split
across frames rather than freezing one. A large project still costs a long frame when its
`Collab` is made, or when a catch-up is materialised; that can't be split without making those
steps themselves incremental.

**`wss://` addresses are accepted.** A page served over https may open only `wss://` sockets.
`relay_address` now accepts `wss://` as well as `ws://`, and join links carry either. Behind a
TLS proxy the relay itself still speaks plain `ws://`; see `docs/install.md`.

- The desktop's client has no TLS. It refuses a `wss://` relay when it connects, with an error
  that says so, rather than at parse time, so a `wss://` link still reads.
- The browser refuses a `ws://` relay from an https page before trying, since the browser would
  block it anyway.

**Joining happens in the tab.** The desktop opens a new window for a join link. The browser has
one window, so it joins in place, and refuses while the tab has changes of its own (history, or
unsaved work): "save them, or join in a new tab". Nothing is opened from a URL. The link is a
capability, and a URL would end up in the browser's history.

**Settings:** the browser has no settings file, so the relay last shared through is not
remembered between visits; the share panel asks for it.
