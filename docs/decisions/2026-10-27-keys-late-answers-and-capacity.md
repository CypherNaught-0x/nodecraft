## 2026-10-27 · Keys panel follows late keychain answers and reserves account places (#240)

The keys panel's timeout no longer ends a request. An OS keychain call can't be cancelled: an
unlock prompt accepted after the panel's 120 s timeout still saves or removes the key. So a timed-out
request stays followed by its `pending` id, and its row says the outcome is unknown (not that it
failed) and can be used again. When the answer comes, a late successful save is remembered in
`[keys] accounts` and a late removal forgotten, whether or not the panel is open. A check that finds
a key (`AccountChange::Found`) lists the account again, so a list that lost an account recovers.

A row shows only the answer to its account's newest request; older answers still update the
remembered accounts. That is right because the keychain worker is one thread answering in request
order, so applying account changes in arrival order matches what the keychain holds. A second
keychain worker, or one that answers out of order, must keep that guarantee or revisit this.

Correlation is bounded at `keys::MAX_PENDING` (256) followed requests. Past it new requests are
refused before dispatch rather than older ones forgotten, so no answer is ever dropped. The host
polls once a second while only timed-out requests remain.

At most `KeySettings::MAX_ACCOUNTS` (64) accounts are remembered. Before a key is written for a
new account, `KeySettings::admits` counts the new accounts whose save is in flight or of unknown
outcome as taken. When the list is full the save is refused before any keychain call. Replacing a
listed or reserved account's key is always allowed, and a failed save releases its place.
`KeySettings::remember` returns whether the account is listed, and the host says so if a list changed
underneath the panel couldn't take it. Rows for accounts that are neither remembered nor awaited are
kept only while there are at most 64 rows, oldest dropped first.
