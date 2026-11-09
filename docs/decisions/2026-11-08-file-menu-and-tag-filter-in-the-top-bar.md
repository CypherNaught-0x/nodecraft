## 2026-11-08 · File and Tag filter lead the top bar; File is a menu (#1123)

The chrome had three control styles at the top left: the framed application row, a small outlined
Tag filter pill alone in a reserved slot, and a square File button under it that opened the
Project files floating panel. File was the only way to open or save, yet it looked like neither
the row nor a menu.

**File and Tag filter are the row's first entries**, in the row's icon-and-label style, with a
divider before Sheets. Tag filter shows its match count beside its label (as Queue shows its
runs) while a filter is active, and opens the existing tag panel below the row, unchanged (#958
redesigns that panel). The reserved Tag filter slot is gone; Format selection keeps the left slot
while several cards are selected.

**Overflow priority** (`Entry::rank`, used by `topbar::fit`): entries move into More by rank,
lowest first and from the right within a rank: the navigation, activity and service entries (0),
an open panel's entry (1), Tag filter (2), Comments then Search (3), and File last (4). File stays
in the row as long as anything does, because it is the only way to open and save; Tag filter goes
before Search and Comments, which #1115 kept in the row down to the smallest window. When File
itself has moved, its menu opens below More.

**File is a menu, not a panel**: an egui popup menu of kind Menu below the File entry (Open…,
Open recent ▸, Save, Save as…, then Save workflow results… and Import folder…), shortcuts on
the right from the commands' own shortcut text (`shortcut::format_shortcut`). It is drawn at the
top of the frame, before the canvas reads input, from the File entry's rectangle of the previous
pass, so the existing File ownership rules (input owned, navigation and model picker refused while
it is open) are unchanged. Its open state is the shell's own (`open_bool`), not egui's popup
memory. A click outside closes it and never reaches the canvas that frame (a click
on another top-bar entry or a panel's control still acts, as it did before); Escape closes it before any
canvas handler reads the key. Entries that can't run stay listed, disabled, with the reason on
hover: without project storage, Open, Save, Save as and the recent projects carry the storage
reason (also shown at the menu's top, with Focus the other window when another window owns
storage); Import folder says a folder is being imported; Save workflow results names the platforms
that have it; Clear recent projects says the list is empty. The browser's File menu is the same
popup with Open…, Save and Save as….

**One panel header**: `floating::panel` draws every floating panel's header itself (egui's title
bar centres the title): the title on the left at `body_size` in the text colour, the close button
(AccessKit "Close window") on the right, a divider below. egui's default fonts have no semibold
face, so the title is set apart by size, colour and the divider rather than weight. The pinned
Shared parameters panel keeps egui's title bar.
