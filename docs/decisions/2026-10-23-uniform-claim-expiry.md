## 2026-10-23 · Claim deadlines apply to owners and maintainers (#793)

The owner and maintainers follow the same claim deadlines as every contributor: open a PR
within 24 hours of the `in progress` label, then push at least every 14 days. This supersedes
the maintainer exemption in [the 24-hour-window decision](2026-10-19-claim-draft-pr-window-24-hours.md).

The daily sweep was succeeding but releasing nothing because all current claims were assigned
to the owner. Abandoned issues stayed claimed indefinitely, even when they had no open PR.
Repository permissions must not make a work reservation permanent.

Both the sweep and `/claim` takeover now use the same expiry calculation for every claimant.
Existing expired claims are eligible on the next sweep; fresh claims and recent PR pushes keep
their existing protection. The daily schedule and the 24-hour/14-day limits are unchanged.
An expired takeover removes and re-adds the label to record the new claimant's start. Even if
an idle PR remains linked, expiry cannot precede that new claim's 24-hour deadline.
Manual `/unclaim` authorization is unchanged: the claimant or a maintainer can release a claim,
and only the owner can manually release an owner-assigned issue.

Regression tests cover expired owner and maintainer claims, takeover, mixed assignees and both
deadline boundaries, including a sweep immediately before and at a takeover's new deadline.
They also retain the manual authorization checks.
