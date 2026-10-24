// Tests for claim.js. Run with `node --test scripts/claim/*.test.js`.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const claim = require('./claim.js');

const NOW = Date.parse('2026-10-16T12:00:00Z');
const H = 3600 * 1000;
const ago = (ms) => new Date(NOW - ms).toISOString();

// A fake github-script client that records writes. `world` describes the repo.
function fake(world = {}) {
  const calls = [];
  const rec = (name) => async (args) => {
    calls.push([name, args]);
    if (world.fail && world.fail[name]) throw Object.assign(new Error(name), { status: world.fail[name] });
    return { data: {} };
  };
  const github = {
    paginate: async (fn, args) => (await fn(args)).data,
    rest: {
      repos: {
        getCollaboratorPermissionLevel: async ({ username }) => {
          const permission = (world.perms || {})[username] || 'none';
          return { data: { permission } };
        },
        getCommit: async (args) => {
          calls.push(['getCommit', args]);
          return { data: { commit: { committer: { date: world.pushed || ago(H) } } } };
        },
      },
      pulls: { get: async (args) => {
        calls.push(['getPull', args]);
        if (world.unavailablePulls && world.unavailablePulls.includes(args.pull_number)) {
          throw Object.assign(new Error('Not Found'), { status: 404 });
        }
        return { data: { head: { sha: 'abc' } } };
      } },
      reactions: { createForIssueComment: rec('react') },
      issues: {
        getLabel: async () => {
          if (world.labelMissing) throw Object.assign(new Error('nf'), { status: 404 });
          return { data: {} };
        },
        createLabel: rec('createLabel'),
        addLabels: rec('addLabels'),
        removeLabel: rec('removeLabel'),
        addAssignees: rec('addAssignees'),
        removeAssignees: rec('removeAssignees'),
        createComment: rec('comment'),
        listForRepo: async () => ({ data: world.issues || [] }),
        listEventsForTimeline: async ({ issue_number }) => ({ data: (world.timelines || {})[issue_number] || world.timeline || [] }),
      },
    },
  };
  return { github, calls };
}

const names = (calls) => calls.map((c) => c[0]);
const find = (calls, n) => calls.find((c) => c[0] === n);
const labeled = (at) => ({ event: 'labeled', label: { name: 'in progress' }, created_at: at });
const pr = (number, login = 'other', state = 'open', owner = 'nodecraft-owner', repository = 'nodecraft') => ({
  event: 'cross-referenced',
  source: { issue: {
    number, state, repository_url: `https://api.github.com/repos/${owner}/${repository}`,
    pull_request: { url: `https://api.github.com/repos/${owner}/${repository}/pulls/${number}` }, user: { login },
  } },
});

function ctx(body, { assignees = [], labels = ['difficulty:medium'], user = 'alice', extra = {} } = {}) {
  return {
    repo: { owner: 'nodecraft-owner', repo: 'nodecraft' },
    payload: {
      issue: {
        number: 7,
        state: 'open',
        locked: false,
        updated_at: ago(1000 * H),
        assignees: assignees.map((login) => ({ login })),
        labels: labels.map((name) => ({ name })),
        ...extra,
      },
      comment: { id: 99, body, user: { login: user, type: 'User' } },
    },
  };
}

test('parseCommand', () => {
  assert.equal(claim.parseCommand('/claim'), 'claim');
  assert.equal(claim.parseCommand('  /claim please'), 'claim');
  assert.equal(claim.parseCommand('/unclaim'), 'unclaim');
  assert.equal(claim.parseCommand('/claimed'), null);
  assert.equal(claim.parseCommand('I would /claim this'), null);
  assert.equal(claim.parseCommand(undefined), null);
});

test('claim on a free issue assigns, labels, reacts and replies', async () => {
  const { github, calls } = fake({ labelMissing: true });
  assert.equal(await claim.handleComment({ github, context: ctx('/claim'), now: NOW }), 'claim');
  assert.deepEqual(find(calls, 'addAssignees')[1].assignees, ['alice']);
  assert.deepEqual(find(calls, 'addLabels')[1].labels, ['in progress']);
  assert.equal(find(calls, 'createLabel')[1].color, 'fbca04');
  assert.equal(find(calls, 'react')[1].content, '+1');
  assert.match(find(calls, 'comment')[1].body, /^Claimed by @alice\. .*draft PR referencing #7 within 24 h/);
});

test('claim still labels and replies when assignment is refused', async () => {
  const { github, calls } = fake({ fail: { addAssignees: 422 } });
  await claim.handleComment({ github, context: ctx('/claim'), now: NOW });
  assert.ok(find(calls, 'addLabels'));
  assert.match(find(calls, 'comment')[1].body, /@alice/);
});

test('claim by the current claimant is a quiet thumbs up', async () => {
  const { github, calls } = fake();
  await claim.handleComment({ github, context: ctx('/claim', { assignees: ['alice'], labels: ['in progress'] }), now: NOW });
  assert.deepEqual(names(calls), ['react']);
});

test('claim on an issue claimed by someone else names them and when it lapses', async () => {
  const { github, calls } = fake({ timeline: [labeled(ago(10 * H))] });
  await claim.handleComment({ github, context: ctx('/claim', { assignees: ['bob'], labels: ['in progress'] }), now: NOW });
  assert.equal(find(calls, 'react')[1].content, 'eyes');
  const body = find(calls, 'comment')[1].body;
  assert.match(body, /@bob/);
  assert.match(body, /2026-10-17 02:00 UTC/); // 10 h ago + 24 h
  assert.equal(find(calls, 'addAssignees'), undefined);
});

test('a lapsed claim is taken over', async () => {
  const { github, calls } = fake({ timeline: [labeled(ago(100 * H))] });
  await claim.handleComment({ github, context: ctx('/claim', { assignees: ['bob'], labels: ['in progress', 'difficulty:medium'] }), now: NOW });
  assert.deepEqual(find(calls, 'removeAssignees')[1].assignees, ['bob']);
  assert.deepEqual(find(calls, 'addAssignees')[1].assignees, ['alice']);
});

test('a claim with a recent PR is not taken over', async () => {
  const { github, calls } = fake({ timeline: [labeled(ago(100 * H)), pr(50, 'bob')], pushed: ago(2 * H) });
  await claim.handleComment({ github, context: ctx('/claim', { assignees: ['bob'], labels: ['in progress'] }), now: NOW });
  assert.equal(find(calls, 'addAssignees'), undefined);
});

for (const holder of ['nodecraft-owner', 'carol']) {
  for (const idlePR of [false, true]) {
    test(`an expired ${holder} claim ${idlePR ? 'with an idle PR' : 'without a PR'} can be taken over`, async () => {
      const world = {
        timeline: [labeled(ago(500 * H)), ...(idlePR ? [pr(9, holder)] : [])],
        pushed: ago(15 * 24 * H),
        perms: { carol: 'write' },
      };
      const { github, calls } = fake(world);
      await claim.handleComment({
        github,
        context: ctx('/claim', { assignees: [holder], labels: ['in progress', 'difficulty:medium'] }),
        now: NOW,
      });
      assert.deepEqual(find(calls, 'removeAssignees')[1].assignees, [holder]);
      assert.deepEqual(find(calls, 'addAssignees')[1].assignees, ['alice']);
      assert.deepEqual(names(calls).filter((name) => ['removeLabel', 'addLabels'].includes(name)),
        ['removeLabel', 'addLabels']);

      // GitHub emits a new labeled event only after removal and re-addition.
      world.timeline.push(labeled(new Date(NOW).toISOString()));
      world.issues = [issue(7, ['alice'])];
      const beforeSweep = calls.length;
      assert.deepEqual(await claim.sweep({ github, context: { repo }, now: NOW + 24 * H - 1 }), []);
      assert.deepEqual(metadataWrites(calls.slice(beforeSweep)), []);
      assert.deepEqual(await claim.sweep({ github, context: { repo }, now: NOW + 24 * H }), [7]);
      assert.deepEqual(calls.filter(([name]) => name === 'removeAssignees').at(-1)[1].assignees, ['alice']);
    });
  }

  test(`a fresh ${holder} claim is protected and reports its deadline`, async () => {
    const { github, calls } = fake({ timeline: [labeled(ago(10 * H))], perms: { carol: 'write' } });
    await claim.handleComment({ github, context: ctx('/claim', { assignees: [holder], labels: ['in progress'] }), now: NOW });
    assert.equal(find(calls, 'addAssignees'), undefined);
    assert.equal(find(calls, 'react')[1].content, 'eyes');
    assert.match(find(calls, 'comment')[1].body, /2026-10-17 02:00 UTC/);
  });
}

test('an unclaimed issue with someone else\'s open PR points at the PR', async () => {
  const { github, calls } = fake({ timeline: [pr(42, 'bob')] });
  await claim.handleComment({ github, context: ctx('/claim'), now: NOW });
  assert.equal(find(calls, 'addAssignees'), undefined);
  assert.match(find(calls, 'comment')[1].body, /#42/);
});

test('bots, locked issues, closed issues, PRs and other comments are ignored', async () => {
  const { github, calls } = fake();
  const bot = ctx('/claim');
  bot.payload.comment.user = { login: 'dependabot[bot]', type: 'Bot' };
  assert.equal(await claim.handleComment({ github, context: bot, now: NOW }), 'skip');
  assert.equal(await claim.handleComment({ github, context: ctx('/claim', { extra: { locked: true } }), now: NOW }), 'skip');
  assert.equal(await claim.handleComment({ github, context: ctx('/claim', { extra: { state: 'closed' } }), now: NOW }), 'skip');
  assert.equal(await claim.handleComment({ github, context: ctx('/claim', { extra: { pull_request: {} } }), now: NOW }), 'skip');
  assert.equal(await claim.handleComment({ github, context: ctx('hello'), now: NOW }), 'skip');
  assert.equal(calls.length, 0);
});

test('unclaim by the claimant releases', async () => {
  const { github, calls } = fake();
  await claim.handleComment({ github, context: ctx('/unclaim', { assignees: ['alice'], labels: ['in progress'] }), now: NOW });
  assert.deepEqual(find(calls, 'removeAssignees')[1].assignees, ['alice']);
  assert.equal(find(calls, 'removeLabel')[1].name, 'in progress');
});

test('unclaim by a maintainer releases, by a stranger does not', async () => {
  let w = fake({ perms: { carol: 'admin' } });
  await claim.handleComment({ github: w.github, context: ctx('/unclaim', { user: 'carol', assignees: ['bob'], labels: ['in progress'] }), now: NOW });
  assert.ok(find(w.calls, 'removeLabel'));
  w = fake();
  await claim.handleComment({ github: w.github, context: ctx('/unclaim', { user: 'mallory', assignees: ['bob'], labels: ['in progress'] }), now: NOW });
  assert.equal(find(w.calls, 'removeLabel'), undefined);
  assert.equal(find(w.calls, 'react')[1].content, 'eyes');
});

test('unclaim on an owner-assigned issue works only for the owner', async () => {
  let w = fake();
  await claim.handleComment({ github: w.github, context: ctx('/unclaim', { user: 'carol', assignees: ['nodecraft-owner'], labels: ['in progress'] }), now: NOW });
  assert.equal(find(w.calls, 'removeLabel'), undefined);
  w = fake();
  await claim.handleComment({ github: w.github, context: ctx('/unclaim', { user: 'nodecraft-owner', assignees: ['nodecraft-owner'], labels: ['in progress'] }), now: NOW });
  assert.ok(find(w.calls, 'removeLabel'));
});

const issue = (number, assignees = ['bob']) => ({
  number,
  locked: false,
  updated_at: ago(1000 * H),
  assignees: assignees.map((login) => ({ login })),
  labels: [{ name: 'in progress' }],
});
const repo = { owner: 'nodecraft-owner', repo: 'nodecraft' };

test('sweep releases a claim older than 24 h with no PR', async () => {
  const { github, calls } = fake({ issues: [issue(7)], timeline: [labeled(ago(80 * H))] });
  assert.deepEqual(await claim.sweep({ github, context: { repo }, now: NOW }), [7]);
  assert.ok(find(calls, 'removeLabel'));
  assert.deepEqual(find(calls, 'removeAssignees')[1].assignees, ['bob']);
  assert.match(find(calls, 'comment')[1].body, /free again/);
});

test('sweep keeps a young claim and a claim with a fresh PR, using the latest label time', async () => {
  let w = fake({ issues: [issue(7)], timeline: [labeled(ago(100 * H)), labeled(ago(5 * H))] });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), []);
  w = fake({ issues: [issue(7)], timeline: [labeled(ago(500 * H)), pr(9)], pushed: ago(3 * 24 * H) });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), []);
});

test('the draft-PR window is exactly 24 h: 23 h is kept, 25 h is released', async () => {
  let w = fake({ issues: [issue(7)], timeline: [labeled(ago(23 * H))] });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), []);
  w = fake({ issues: [issue(7)], timeline: [labeled(ago(25 * H))] });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), [7]);
  assert.match(find(w.calls, 'comment')[1].body, /No draft PR showed up within 24 h/);
});

test('sweep releases a claim whose PR has been idle for 14 days', async () => {
  const w = fake({ issues: [issue(7)], timeline: [labeled(ago(500 * H)), pr(9)], pushed: ago(15 * 24 * H) });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), [7]);
  assert.match(find(w.calls, 'comment')[1].body, /14 days/);
});

test('sweep ignores a closed PR', async () => {
  const w = fake({ issues: [issue(7)], timeline: [labeled(ago(80 * H)), pr(9, 'other', 'closed')] });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), [7]);
});

test('sweep releases expired owner, maintainer and mixed-assignee claims', async () => {
  const w = fake({
    issues: [issue(1, ['nodecraft-owner']), issue(2, ['carol']), issue(3, ['bob', 'carol'])],
    timeline: [labeled(ago(900 * H))],
    perms: { carol: 'write' },
  });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), [1, 2, 3]);
  assert.deepEqual(w.calls.filter(([name]) => name === 'removeAssignees').map(([, args]) => args.assignees),
    [['nodecraft-owner'], ['carol'], ['bob', 'carol']]);
});

for (const holder of ['nodecraft-owner', 'carol']) {
  for (const { labelAge, pushAge, expires } of [
    { labelAge: 24 * H - 1, expires: false },
    { labelAge: 24 * H, expires: true },
    { labelAge: 500 * H, pushAge: 14 * 24 * H - 1, expires: false },
    { labelAge: 500 * H, pushAge: 14 * 24 * H, expires: true },
  ]) {
    test(`sweep ${expires ? 'releases' : 'keeps'} ${holder} at ${pushAge === undefined ? `claim age ${labelAge}` : `PR age ${pushAge}`}`, async () => {
      const { github, calls } = fake({
        issues: [issue(7, [holder])],
        timeline: [labeled(ago(labelAge)), ...(pushAge === undefined ? [] : [pr(9, holder)])],
        pushed: pushAge === undefined ? undefined : ago(pushAge),
        perms: { carol: 'write' },
      });
      assert.deepEqual(await claim.sweep({ github, context: { repo }, now: NOW }), expires ? [7] : []);
      if (expires) {
        assert.deepEqual(find(calls, 'removeAssignees')[1].assignees, [holder]);
        assert.match(find(calls, 'comment')[1].body, pushAge === undefined ? /within 24 h/ : /14 days/);
      } else {
        assert.deepEqual(metadataWrites(calls), []);
      }
    });
  }
}

test('#596 foreign craft-libs#240 does not block an expired NodeCraft claim', async () => {
  const w = fake({ issues: [issue(596)], timeline: [labeled(ago(80 * H)), pr(240, 'upstream', 'open', 'storytold', 'craft-libs')], unavailablePulls: [240] });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), [596]);
  assert.equal(find(w.calls, 'getPull'), undefined);
});

test('local and foreign PRs with the same number cannot collide; recent local PR protects the claim', async () => {
  const timeline = [labeled(ago(500 * H)), pr(240, 'local'), pr(240, 'upstream', 'open', 'storytold', 'craft-libs')];
  const w = fake({ issues: [issue(596)], pushed: ago(H), timeline });
  assert.deepEqual(claim.linkedPulls(timeline, { repo }).map((p) => p.user.login), ['local']);
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), []);
  assert.deepEqual(w.calls.filter(([name]) => name === 'getPull').map(([, args]) => args),
    [{ ...repo, pull_number: 240 }]);
  assert.equal(find(w.calls, 'removeLabel'), undefined);
});

test('an unavailable local PR holds only its issue and reports it while other claims expire', async () => {
  const warnings = [];
  const w = fake({ issues: [issue(596), issue(597)], unavailablePulls: [240],
    timelines: { 596: [labeled(ago(500 * H)), pr(240)], 597: [labeled(ago(80 * H))] },
  });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW, report: (text) => warnings.push(text) }), [597]);
  assert.deepEqual(warnings.length, 1);
  assert.match(warnings[0], /Skipped issue #596: Cannot resolve local PR #240/);
  assert.deepEqual(w.calls.filter(([name]) => name === 'removeLabel').map(([, args]) => args.issue_number), [597]);
});

test('missing repository identity holds the uncertain claim but sweeps unrelated issues', async () => {
  const warnings = [];
  const unknown = pr(240);
  delete unknown.source.issue.repository_url;
  delete unknown.source.issue.pull_request.url;
  const w = fake({ issues: [issue(596), issue(597)],
    timelines: { 596: [labeled(ago(80 * H)), unknown], 597: [labeled(ago(80 * H))] },
  });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW, report: (text) => warnings.push(text) }), [597]);
  assert.match(warnings[0], /issue #596: Cannot identify repository for open PR #240/);
  assert.equal(find(w.calls, 'getPull'), undefined);
});

test('conflicting repository identities cannot release a claim', async () => {
  const conflict = pr(240);
  conflict.source.issue.pull_request.url = 'https://api.github.com/repos/storytold/craft-libs/pulls/240';
  const warnings = [];
  const w = fake({ issues: [issue(596)], timeline: [labeled(ago(80 * H)), conflict] });
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW, report: (text) => warnings.push(text) }), []);
  assert.match(warnings[0], /issue #596: Cannot identify repository/);
  assert.equal(find(w.calls, 'getPull'), undefined);
});

test('foreign upstream PR cannot block a new NodeCraft claim', async () => {
  const w = fake({ timeline: [pr(240, 'upstream', 'open', 'storytold', 'craft-libs')] });
  await claim.handleComment({ github: w.github, context: ctx('/claim'), now: NOW });
  assert.ok(find(w.calls, 'addAssignees'));
  assert.equal(find(w.calls, 'getPull'), undefined);
});

test('unavailable local PR prevents an uncertain takeover', async () => {
  const w = fake({ timeline: [labeled(ago(500 * H)), pr(240)], unavailablePulls: [240] });
  await claim.handleComment({ github: w.github, context: ctx('/claim', { assignees: ['bob'], labels: ['in progress'] }), now: NOW });
  assert.equal(find(w.calls, 'removeLabel'), undefined);
  assert.match(find(w.calls, 'comment')[1].body, /Cannot verify this claim.*local PR #240/);
});

test('sweep is idempotent: a released issue is no longer listed', async () => {
  const world = { issues: [issue(7)], timeline: [labeled(ago(80 * H))] };
  const w = fake(world);
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), [7]);
  world.issues = [];
  assert.deepEqual(await claim.sweep({ github: w.github, context: { repo }, now: NOW }), []);
});


const metadataWrites = (calls) => names(calls).filter((name) =>
  ['createLabel', 'addLabels', 'removeLabel', 'addAssignees', 'removeAssignees'].includes(name),
);

for (const level of ['easy', 'medium', 'hard']) {
  for (const representation of ['objects', 'strings']) {
    test(`claim accepts ${level} difficulty as ${representation} and ignores unrelated labels`, async () => {
      const { github, calls } = fake();
      const labels = ['area:infra', `difficulty:${level}`, 'enhancement'];
      const context = ctx('/claim', { labels });
      if (representation === 'strings') context.payload.issue.labels = labels;
      await claim.handleComment({ github, context, now: NOW });
      assert.deepEqual(find(calls, 'addAssignees')[1].assignees, ['alice']);
      assert.deepEqual(find(calls, 'addLabels')[1].labels, ['in progress']);
    });
  }
}

for (const labels of [
  [],
  ['area:infra'],
  ['difficulty:'],
  ['difficulty:unknown'],
  ['difficulty:easy', 'difficulty:hard'],
  ['difficulty:medium', 'difficulty:unknown'],
]) {
  for (const lapsed of [false, true]) {
    test(`invalid difficulty ${JSON.stringify(labels)} preserves metadata${lapsed ? ' and lapsed owner' : ''}`, async () => {
      const { github, calls } = fake({ labelMissing: true, timeline: [labeled(ago(100 * H))] });
      const context = ctx('/claim', {
        labels: lapsed ? [...labels, 'in progress'] : labels,
        assignees: lapsed ? ['bob'] : [],
      });
      await claim.handleComment({ github, context, now: NOW });
      assert.deepEqual(metadataWrites(calls), []);
      assert.equal(find(calls, 'react')[1].content, 'eyes');
      assert.match(find(calls, 'comment')[1].body, /exactly one difficulty label/);
      assert.match(find(calls, 'comment')[1].body, /docs\/issue-labels\.md/);
      assert.match(find(calls, 'comment')[1].body, /retry `\/claim`/);
    });
  }
}
