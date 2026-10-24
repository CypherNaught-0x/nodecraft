// The /claim bot's logic, used by .github/workflows/claim.yml through actions/github-script.
// Everything takes the github-script `github` client and `context`, so the tests can pass fakes.
// Rules for contributors: CONTRIBUTING.md, "Claiming an issue".
//
// Adapted from SolidCraft's scripts/claim/claim.js (MIT OR Apache-2.0, Copyright (c) 2026
// SolidCraft contributors), https://github.com/bherbruck/solidcraft at
// 77b33efd8f0e8498f4d0d00697419d0992150759. See ATTRIBUTION.md.
'use strict';

const LABEL = 'in progress';
const LABEL_COLOR = 'fbca04';
const LABEL_DESCRIPTION = 'Someone is working on this';
const CLAIM_HOURS = 24;
const PR_IDLE_DAYS = 14;
const HOUR = 3600 * 1000;

function parseCommand(body) {
  const text = String(body || '').trimStart();
  if (/^\/claim(\s|$)/.test(text)) return 'claim';
  if (/^\/unclaim(\s|$)/.test(text)) return 'unclaim';
  return null;
}

function fmt(date) {
  return new Date(date).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
}

function repoArgs(context) {
  return { owner: context.repo.owner, repo: context.repo.repo };
}

async function isMaintainer(github, context, login, cache) {
  if (login === context.repo.owner) return true;
  if (cache && cache.has(login)) return cache.get(login);
  let result = false;
  try {
    const { data } = await github.rest.repos.getCollaboratorPermissionLevel({
      ...repoArgs(context),
      username: login,
    });
    result =
      ['admin', 'write', 'maintain'].includes(data.permission) ||
      ['admin', 'maintain', 'write'].includes(data.role_name);
  } catch (e) {
    result = false;
  }
  if (cache) cache.set(login, result);
  return result;
}

async function ensureLabel(github, context) {
  try {
    await github.rest.issues.getLabel({ ...repoArgs(context), name: LABEL });
  } catch (e) {
    if (e.status !== 404) throw e;
    try {
      await github.rest.issues.createLabel({
        ...repoArgs(context),
        name: LABEL,
        color: LABEL_COLOR,
        description: LABEL_DESCRIPTION,
      });
    } catch (e2) {
      if (e2.status !== 422) throw e2; // created meanwhile
    }
  }
}

// When the label was last added, from the timeline; the issue's update time if it can't be found.
async function labeledAt(github, context, issue, timeline) {
  const events = timeline || (await getTimeline(github, context, issue.number));
  let at = null;
  for (const ev of events) {
    if (ev.event === 'labeled' && ev.label && ev.label.name === LABEL) at = ev.created_at;
  }
  return new Date(at || issue.updated_at).getTime();
}

function getTimeline(github, context, number) {
  return github.paginate(github.rest.issues.listEventsForTimeline, {
    ...repoArgs(context),
    issue_number: number,
    per_page: 100,
  });
}

// Only PRs in this repository claim its issues. Timeline sources can be foreign PRs
// with the same number as a local PR; never infer repository identity from the number.
function linkedPulls(timeline, context) {
  const seen = new Map();
  for (const ev of timeline) {
    const src = ev.event === 'cross-referenced' && ev.source && ev.source.issue;
    if (!src || !src.pull_request || src.state !== 'open') continue;
    const repository = /^https:\/\/api\.github\.com\/repos\/([^/]+)\/([^/]+)\/?$/i.exec(src.repository_url || '');
    const pull = /^https:\/\/api\.github\.com\/repos\/([^/]+)\/([^/]+)\/pulls\/(\d+)\/?$/i.exec(src.pull_request.url || '');
    if ((!repository && !pull) || (src.repository_url && !repository) || (src.pull_request.url && !pull) ||
        (pull && String(src.number) !== pull[3]) ||
        (repository && pull && (repository[1].toLowerCase() !== pull[1].toLowerCase() ||
          repository[2].toLowerCase() !== pull[2].toLowerCase()))) {
      throw new Error(`Cannot identify repository for open PR #${src.number || '?'}; keeping claim`);
    }
    const identity = repository || pull;
    if (identity[1].toLowerCase() !== context.repo.owner.toLowerCase() ||
        identity[2].toLowerCase() !== context.repo.repo.toLowerCase()) continue;
    seen.set(src.number, src);
  }
  return [...seen.values()];
}

async function lastPush(github, context, pr) {
  const { data } = await github.rest.pulls.get({ ...repoArgs(context), pull_number: pr.number });
  const { data: commit } = await github.rest.repos.getCommit({
    ...repoArgs(context),
    ref: data.head.sha,
  });
  return new Date(commit.commit.committer.date).getTime();
}

// Where a claimed issue stands: when it lapses, and whether it is free to release now.
async function claimStatus(github, context, issue, now) {
  const timeline = await getTimeline(github, context, issue.number);
  const since = await labeledAt(github, context, issue, timeline);
  const pulls = linkedPulls(timeline, context);
  if (pulls.length === 0) {
    const expires = since + CLAIM_HOURS * HOUR;
    return { pulls, since, expires, release: now >= expires, reason: 'no PR' };
  }
  let newest = 0;
  for (const pr of pulls) {
    try {
      newest = Math.max(newest, await lastPush(github, context, pr));
    } catch (e) {
      throw new Error(`Cannot resolve local PR #${pr.number}; keeping claim`, { cause: e });
    }
  }
  // An expired PR from a previous claimant must not erase a new claim's first day.
  const expires = Math.max(since + CLAIM_HOURS * HOUR, newest + PR_IDLE_DAYS * 24 * HOUR);
  return { pulls, since, expires, release: now >= expires, reason: 'idle PR' };
}

async function react(github, context, content) {
  await github.rest.reactions.createForIssueComment({
    ...repoArgs(context),
    comment_id: context.payload.comment.id,
    content,
  });
}

async function reply(github, context, body) {
  await github.rest.issues.createComment({
    ...repoArgs(context),
    issue_number: context.payload.issue.number,
    body,
  });
}

function hasLabel(issue) {
  return (issue.labels || []).some((l) => (typeof l === 'string' ? l : l.name) === LABEL);
}

async function release(github, context, issue, assignees) {
  if (assignees.length) {
    await github.rest.issues.removeAssignees({
      ...repoArgs(context),
      issue_number: issue.number,
      assignees,
    });
  }
  try {
    await github.rest.issues.removeLabel({
      ...repoArgs(context),
      issue_number: issue.number,
      name: LABEL,
    });
  } catch (e) {
    if (e.status !== 404) throw e;
  }
}

async function claim(github, context, now) {
  const issue = context.payload.issue;
  const user = context.payload.comment.user.login;
  const n = issue.number;
  const assignees = (issue.assignees || []).map((a) => a.login);

  if (assignees.includes(user)) {
    await react(github, context, '+1'); // already theirs
    return;
  }

  // Every claimant has the same deadline, including owners and maintainers.
  let held = assignees.length > 0 || hasLabel(issue);
  let takeover = false;
  if (held) {
    let status;
    try {
      status = await claimStatus(github, context, issue, now);
    } catch (e) {
      await react(github, context, 'eyes');
      await reply(github, context, `Cannot verify this claim (${e.message}). It stays held; ask a maintainer to check the PR reference.`);
      return;
    }
    if (status.release) {
      held = false;
      takeover = true;
    }
    if (held) {
      await react(github, context, 'eyes');
      const who = assignees.length ? assignees.map((a) => '@' + a).join(', ') + ' has' : 'Someone has';
      const when = ` The claim lapses on ${fmt(status.expires)} unless there's progress.`;
      await reply(github, context, `${who} claimed this already.${when}`);
      return;
    }
  } else {
    // Not claimed, but a draft PR may already be the claim (CONTRIBUTING.md, "Claiming an issue").
    const timeline = await getTimeline(github, context, n);
    let other;
    try {
      other = linkedPulls(timeline, context).find((p) => p.user && p.user.login !== user);
    } catch (e) {
      await react(github, context, 'eyes');
      await reply(github, context, `Cannot verify the open PR reference (${e.message}). Ask a maintainer to check it before claiming.`);
      return;
    }
    if (other) {
      await react(github, context, 'eyes');
      await reply(github, context, `#${other.number} already covers this one.`);
      return;
    }
  }

  // Validate before clearing a lapsed owner or changing any issue metadata.
  const difficulties = (issue.labels || [])
    .map((label) => (typeof label === 'string' ? label : label.name))
    .filter((name) => typeof name === 'string' && name.startsWith('difficulty:'));
  if (difficulties.length !== 1 || !['difficulty:easy', 'difficulty:medium', 'difficulty:hard'].includes(difficulties[0])) {
    await react(github, context, 'eyes');
    await reply(
      github,
      context,
      'Before claiming, ask a maintainer to set exactly one difficulty label: `difficulty:easy`, `difficulty:medium` or `difficulty:hard`. ' +
        `See the [difficulty rubric](https://github.com/${context.repo.owner}/${context.repo.repo}/blob/main/docs/issue-labels.md). Then retry \`/claim\`.`,
    );
    return;
  }

  // Free (or lapsed): hand it to the commenter. Clear a lapsed claimant first.
  // Removing and re-adding the label records the new claimant's start in the timeline.
  if (takeover) await release(github, context, issue, assignees);
  await ensureLabel(github, context);
  await github.rest.issues.addLabels({ ...repoArgs(context), issue_number: n, labels: [LABEL] });
  try {
    await github.rest.issues.addAssignees({
      ...repoArgs(context),
      issue_number: n,
      assignees: [user],
    });
  } catch (e) {
    // Assignment can be refused; the label and the reply naming them still stand.
  }
  await react(github, context, '+1');
  await reply(
    github,
    context,
    `Claimed by @${user}. Please open a draft PR referencing #${n} within ${CLAIM_HOURS} h; the claim lapses otherwise. \`/unclaim\` releases it.`,
  );
}

async function unclaim(github, context) {
  const issue = context.payload.issue;
  const user = context.payload.comment.user.login;
  const assignees = (issue.assignees || []).map((a) => a.login);
  if (!hasLabel(issue) && assignees.length === 0) {
    await react(github, context, '+1'); // nothing to release
    return;
  }
  if (assignees.includes(context.repo.owner) && user !== context.repo.owner) {
    await react(github, context, 'eyes'); // only the owner releases their own issues
    return;
  }
  const allowed = assignees.includes(user) || (await isMaintainer(github, context, user));
  if (!allowed) {
    await react(github, context, 'eyes');
    await reply(github, context, `Only the person who claimed this, or a maintainer, can release it, @${user}.`);
    return;
  }
  await release(github, context, issue, assignees);
  await react(github, context, '+1');
  await reply(github, context, `Released. This one is free again, so anyone can \`/claim\` it.`);
}

// The comment event.
async function handleComment({ github, context, now = Date.now() }) {
  const { issue, comment } = context.payload;
  if (!issue || issue.pull_request || !comment) return 'skip';
  if (issue.state !== 'open' || issue.locked) return 'skip';
  if (comment.user.type === 'Bot' || comment.user.login.endsWith('[bot]')) return 'skip';
  const cmd = parseCommand(comment.body);
  if (!cmd) return 'skip';
  if (cmd === 'claim') await claim(github, context, now);
  else await unclaim(github, context);
  return cmd;
}

// The daily sweep over open issues labelled `in progress`.
async function sweep({ github, context, now = Date.now(), report = console.warn }) {
  const issues = await github.paginate(github.rest.issues.listForRepo, {
    ...repoArgs(context),
    state: 'open',
    labels: LABEL,
    per_page: 100,
  });
  const released = [];
  for (const issue of issues) {
    if (issue.pull_request || issue.locked) continue;
    const assignees = (issue.assignees || []).map((a) => a.login);
    let status;
    try {
      status = await claimStatus(github, context, issue, now);
    } catch (e) {
      report(`Skipped issue #${issue.number}: ${e.message}`);
      continue;
    }
    if (!status.release) continue;
    await release(github, context, issue, assignees);
    await github.rest.issues.createComment({
      ...repoArgs(context),
      issue_number: issue.number,
      body:
        status.reason === 'no PR'
          ? `No draft PR showed up within ${CLAIM_HOURS} h, so this one is free again. Anyone can \`/claim\` it.`
          : `Nothing has been pushed for ${PR_IDLE_DAYS} days, so this one is free again. Anyone can \`/claim\` it.`,
    });
    released.push(issue.number);
  }
  return released;
}

module.exports = {
  LABEL,
  parseCommand,
  isMaintainer,
  linkedPulls,
  claimStatus,
  handleComment,
  sweep,
};
