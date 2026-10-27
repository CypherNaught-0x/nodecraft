## 2026-10-27 · One top-bar control list and overflow (#956)

Build the application menu from one ordered list of controls. Each control declares its Tabler
icon, label, group, availability, active state, optional count and tooltip. The wide row and
the narrow More menu render that same list and call the same action handler. Navigation,
activity, settings/services and Search/Comments keep their order; dividers separate groups.
Unavailable host services remain visible and disabled. Browser provider settings retain their
additional Providers entry. Open as app appears when a Technique is selected.

Measure the complete row, including icons, counts, spacing and dividers. If it does not fit,
put the complete list into one scrollable More menu instead of clipping labels. Keyboard focus
follows rendering order and skips disabled controls. A dirty editor or a canvas gesture blocks
opening More, so its opening click cannot commit an unrelated draft. Sheets draw and protect
the chrome before the table can process a prompt losing focus. Search and Comments
continue through their existing engine service commands with fresh input-ownership checks.
Only this menu's popup is admitted when computing its own controls' availability.

Reserve left slots for the existing Tag filter control and, when present, Format selection.
Their width is included before admitting the application row; an open Tags panel stays below
both controls. The tag-filter redesign remains separate. Search's panel
opens below the row, and discussions open below search when both are visible. Search defers its
focus request until its input is visibly drawn, including the first Area sizing pass. A later
explicit Search or Tags open cancels the other panel's pending focus request.

Keep sharing status in the Share tooltip and collaborator avatars inside Share's panel. This
keeps a variable number of avatars from invalidating the row's fit measurement or covering other
controls. Panel changes remain shell view state; document changes retain their existing commands.

Extend the finite icon catalog only with the ten SVGs used here, unchanged from the same Tabler
commit selected for #955. Keep their existing MIT license and add each source to ATTRIBUTION.md.
The common renderer uses existing theme tokens in both palettes and adds no network access.
