## 2026-10-26 · Local text action previews (#969)

Cards whose descriptor is an Action with only Text outputs reserve two existing preview-height
regions together: computed output and connected inputs. This covers Join text, Template, Split
text, Replace text and compatible installed actions. Geometry is independent of results, errors
and expansion, so live recompute does not move ports. At overview zoom the usual card-body
threshold still applies.

The shell caches excerpts by session identity, registry, canvas and document revision. It reads
the stored active result rather than running an action for its preview. Ordinary Text source
values stay borrowed; other source kinds use their pure source plan. Resolved router feeds and
their ordering come from the same routing function the planner uses. Batch items use the existing
batch parser, including upstream values; list source outputs retain every matching source job.
Batch traversal is cycle/depth guarded and shares a value/byte budget per read. Expanded lists
preserve leading and trailing empty text items. Input hashes share one
bounded hasher to validate retained failures and mark older outputs as updating.

Excerpts take at most 256 characters and three lines; rendering further limits them to three
wrapped rows. A virtualized list draws only the rows in view. The cache admits at most 16,384
rows and 8 MiB including charged row overhead, plus one fixed omission notice. Its temporary
source-summary memo also has at most one entry per admitted input row. Larger graphs show an
explicit limit message. No preview is refreshed for an uncommitted field draft or an idle frame.

Expand opens one modal, selectable full-value view, bounded to 8 MiB and 16,384 list values.
Only this explicit request copies full text; normal frames format the cached excerpts. The modal
updates on document changes and owns canvas input through its closing frame. It creates no
document change or undo entry. Empty text and lists are distinguished from absent results.

Live failures remain view state, capped at 4,096 messages of 1,024 characters. A failure is only
accepted from a round for the current canvas and revision, and shown while its input hash still
matches. A changed recipe hides the old error immediately. Missing connections and invalid
routers are shown beside their input; action errors name the relevant input ports.
