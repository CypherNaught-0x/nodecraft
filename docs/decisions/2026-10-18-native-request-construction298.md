## 2026-10-18 · Native request construction is terminal (Part of #298)

Reqwest `RequestBuilder::build` failures occur before native execution. Classify this boundary
as `UnsupportedInput` with a fixed diagnostic, so the scheduler avoids futile retries and the
audited LLM submission can release a fresh reservation using its existing complete-operation
evidence. Do not derive billing certainty from the error kind: prior ambiguous attempts and
unaudited middleware remain conservative, and native execution failures retain existing retry
classification. Browser fetch does not gain positive non-delivery evidence. Reconciliation and
user-facing release notes remain separate work under #298.
