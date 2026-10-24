## 2026-10-22 · Preserve built-in run-refusal explanations (#655)

`run.quote` and `run.start` retain the actionable validation explanations produced by the
planner and NodeCraft's built-in node implementations. For example, a style-family mismatch
names both families and suggests choosing a matching model or retraining the style. Both
commands use the same explanation before any work is admitted.

This narrows the static-only reporting policy in
[the option-list decision](2026-10-20-option-lists-block-a-run.md). Option-list and endpoint
failures still use their fixed messages. Arbitrary native callbacks remain private: their
validation failures produce the generic `node_validation` explanation. Registry provenance
is attached by the built-in factory or the concrete generation-kind registration method;
generic registration and replacement cannot acquire it by reusing a built-in identifier.
Technique remapping preserves this distinction without copying an inner card's title into a
public validation explanation.

Public explanations can contain the relevant model, port or mentioned-card names selected
by the built-in implementation. They never copy an arbitrary native error, provider response
or debug dump. Each retained explanation is limited to 1,024 UTF-8 bytes at a character
boundary. The existing 100-record and 16-KiB shared report limits still apply, including the
`run.start` separators and omission trailer. Tests cover family mismatch through both commands,
replacement of a built-in identifier by a private callback, Unicode truncation and the shared
report limit.
