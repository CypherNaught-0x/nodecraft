## 2026-10-22 · Browser keys per origin (#118)

- **The browser build keeps provider keys in `localStorage` for its origin**, one entry per
  account named `nodecraft.key.<account>` (`secrets::OriginStore`). This replaces the in-memory
  store the 2026-10-17 keychain record names for the browser. `sessionStorage` would lose keys on
  every reload. IndexedDB would add an asynchronous store without protecting anything more,
  since any script on the origin can read either one.
- **The panel says so.** The text at the top of the API keys panel names this browser and site,
  says anyone using the browser profile can read the keys, and asks people to remove them on a
  shared computer. Keys still never go into projects, settings, logs or error messages. A stored
  value that isn't a usable key is reported as damaged and is never echoed.
- **Engine logic, host storage.** The engine owns the naming, listing and checks behind an
  `OriginStorage` trait, so they are tested natively. The egui shell implements that trait over
  `web_sys::Storage`, looking the storage up on each call so the store holds no browser object.
  Calls answer at once, so the browser registers the synchronous `keys.store` service, not the
  keychain worker.
- **Accounts are listed from the stored entries**, because the browser has no settings file to
  remember them in. Only names that are valid accounts are listed, and the rest of the origin's
  data is left alone.
