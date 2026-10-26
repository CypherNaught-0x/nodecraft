## 2026-10-26 · Preserve rendered media when Timeline provenance fails (#578)

Timeline source records describe a completed render; they are optional bookkeeping, rather than
part of the media itself. An extraction error or record-budget refusal must not turn a successful
render into a failed step and discard its output.

Run assembly admits source records against one shared generation-wide limit before extending
the retained per-item vectors. If extraction fails or the total would exceed 65,536 records,
it drops all source records for that result and stops collecting further records. This avoids
presenting a partial set as complete. Resumed items use the same budget. Final recording can
also shed optional provenance if the result cannot fit alongside its other metadata.

The output, settings that fit, and normal generation validation remain intact. A fixed warning
reports omitted provenance without logging recipe parameters or extraction errors, which may
contain private inputs. The editor's existing source-history display remains available for
records that were retained; an empty record set makes no claim that a source hash was captured.

Malformed externally supplied generations and archives still fail their normal validation.
This fallback applies to optional metadata produced while assembling an otherwise successful
run. Regressions retain the exact completed Timeline output after extraction failure and after
oversized metadata, and exercise the shared cap and resumed-item record budget.
