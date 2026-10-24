## 2026-10-24 · Saved 3D viewer presentation (#928)

The viewer in #111 originally kept its camera and shading only in the shell. The saved-transform
acceptance in #569 changes that for Source 3D, Generate 3D and Convert 3D. Four node-owned
parameters store yaw/elevation in degrees, zoom and shading. Old projects without them keep the
original three-quarter view. `mesh.view_set` validates and changes all four through one changeset,
so command clients and pointer gestures use the same undo and persistence path.

The renderer retains a transient preview while dragging or scrolling. Release ends a drag;
250 ms of scroll inactivity ends an Alt-scroll group. Saving finishes any pending viewer edit
before taking a snapshot. Undo or an external parameter change replaces an outdated local
preview. Failed edits restore the saved camera and report the command error. Opening a project
clears the previous project's viewer cache.

These are presentation parameters: their contract affects neither identity, pricing, shape nor
job count. Source emission and deterministic conversion omit them from jobs, and generation
keeps them outside the provider-owned parameter group. Model changes preserve them. Generation
hashing also omits these four new node-owned controls from its serialized contract, preserving
the exact older recipe representation. A regression compares an old descriptor/contract with
the new one, then checks unchanged jobs and hashes after moving the viewer.

This supersedes only the viewer-state portion of the earlier [3D node decision](2026-10-21-3d-node.md).
Photo, Views and Turntable still seed their own saved capture parameters from the current view;
later viewer changes do not alter existing captures. Other mesh-producing extension kinds keep
their previous transient viewer behavior until they declare a presentation contract.

This slice does not add STL/PLY/USDZ, preserve UV/material structures, or implement a browser
worker. Those remaining #569 requirements stay open and require separate bounded format and
worker designs.
