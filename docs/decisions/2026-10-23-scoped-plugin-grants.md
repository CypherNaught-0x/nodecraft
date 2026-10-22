## 2026-10-23 · Plugin grants are scoped to what the person saw (#174)

**Decision.** A plugin grant records its package digest, the capabilities it covers and, with
network, the exact origins the package's provider templates reach. `plugins.rights` shows these;
`plugins.grant` must repeat them exactly (digest, every required capability plus chosen optional
ones, every origin), or it is refused. Granting other rights revokes the live grant and issues a
new epoch. Updates report a `rights_delta` and never carry a grant over. The store (layer 1)
records and checks rights; the engine (layer 4) reads the templates to compute origins, because
template decoding lives in the provider broker (layer 3).

**Why.** A yes covers what the person was shown and nothing more: a grant that silently widened to
a new origin, or a request that was quietly narrowed, would make the consent dialog meaningless.
Repeating the digest closes the race where the package changes between showing and granting.
Network without its origins is all of the internet, so the two are only granted together.

**Compatibility.** `grants.json` stays version 1 while every grant is plain, so builds from before
#174 keep reading stores with only text action grants; scoped grants write version 2, which those
builds refuse as newer rather than misreading.

**Not chosen.** Granting network per request at run time (prompts during paid runs, and agents
could trigger them); recording origins in the manifest (they would duplicate the templates and
could disagree with them).
