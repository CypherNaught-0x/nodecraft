# 2026-10-17 · Live canvas navigation (#62)

- Navigation stays shell view state. Stable engine commands produce bounded service intents, with
  no graph changes, history or events. The app bridge supplies a fresh scoped capability and
  restores any previous resource even on dispatch failure; raw headless dispatch is Unsupported.
- Fit and minimap geometry resolve real instances once per revision and retain only bounded node
  rectangles. They never rely on the evictable visible-card schema cache or static palette metadata.
  The minimum zoom makes very spread graphs best effort, with an explicit notice.
- Shift+1/2 fit all/selected; Shift+M toggles the map without macOS's Command+M conflict. Editors,
  popups and existing gestures own their input. A minimap drag freezes its world mapping and owns
  pointer release outside its bounds. Visibility is session-local, not serialized settings.
