import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { readWorkspaceFile } from '../file-preview.mjs';
import { readerURL, workspaceLink } from '../public/link-policy.js';
import { linkPreviewSource } from '../public/link-preview.js';
import { androidDocURL } from '../android-docs.mjs';

const source = (url) => linkPreviewSource(new URL(url));

test('GitHub links resolve only to the public preview endpoints', () => {
  assert.deepEqual(source('https://github.com/owner/repository/pull/42/files?ignored=1#fragment'), {
    kind: 'diff', label: 'owner/repository #42',
    endpoint: 'https://api.github.com/repos/owner/repository/pulls/42/files',
  });
  const article = source('https://docs.github.com/zh/enterprise-cloud@latest/get-started/using-github-docs/github-docs-api/#examples');
  assert.equal(article.kind, 'article');
  assert.equal(new URL(article.endpoint).origin, 'https://docs.github.com');
  assert.equal(new URL(article.endpoint).pathname, '/api/article/body');
  assert.equal(new URL(article.endpoint).searchParams.get('pathname'),
    '/zh/enterprise-cloud@latest/get-started/using-github-docs/github-docs-api');
  assert.deepEqual(source('https://github.com/owner/repository/?tab=readme-ov-file#usage'), {
    kind: 'readme', endpoint: 'https://api.github.com/repos/owner/repository/readme',
  });
  assert.equal(source('https://github.com/sindresorhus/modern-normalize').endpoint,
    'https://api.github.com/repos/sindresorhus/modern-normalize/readme');
  assert.equal(source('https://github.com/owner/repository/issues'), null);
  assert.equal(source('https://github.com:8080/owner/repository/pull/42/files'), null);
  assert.equal(source('https://secret@github.com/owner/repository/pull/42/files'), null);
  assert.equal(source('https://docs.github.com/api/article/body'), null);
  assert.equal(source('https://example.com/owner/repository/pull/42/files'), null);
});

test('unrecognized HTTPS links and Android app intents are left to the browser', () => {
  assert.equal(source('https://maps.app.goo.gl/example'), null);
  assert.equal(source('https://play.google.com/store/apps/details?id=example.app'), null);
  assert.equal(source('https://github.com/apps/example'), null);
  assert.equal(source('https://github.com/owner/repository/pull/42'), null);
  assert.equal(source('intent://example/#Intent;scheme=https;end'), null);
  assert.equal(source('http://github.com/owner/repository'), null);
});

test('web link modes share a strict reader rule and leave native intents outside iframes', () => {
  const guide = new URL('https://web.dev/articles/manifest-updates?hl=zh-cn&unrelated=1#android');
  const preview = linkPreviewSource(guide);
  assert.equal(preview.kind, 'reader');
  assert.equal(new URL(preview.endpoint, 'http://localhost').searchParams.get('url'),
    'https://web.dev/articles/manifest-updates?hl=zh-cn');
  assert.equal(linkPreviewSource(guide, 'browser'), null);
  assert.deepEqual(linkPreviewSource(guide, 'iframe'), { kind: 'iframe' });
  assert.deepEqual(linkPreviewSource(new URL('http://example.com/'), 'iframe'), { kind: 'iframe' });
  assert.equal(linkPreviewSource(new URL('intent://maps/#Intent;end'), 'iframe'), null);
  for (const url of ['https://web.dev/', 'https://web.dev.evil.example/articles/test', 'https://web.dev:8001/articles/test',
    'https://web.dev/articles/%2fevil', 'https://login@web.dev/articles/test']) assert.equal(readerURL(new URL(url)), null);
});

test('file links preserve the path and supported line anchors', () => {
  assert.deepEqual(workspaceLink('file:///tmp/a%20b.md#L21-L23'), { path: '/tmp/a b.md', line: 21 });
  assert.deepEqual(workspaceLink('/workbench/a.js:4:9'), { path: '/workbench/a.js', line: 4 });
  assert.deepEqual(workspaceLink('src/a.js'), { path: 'src/a.js', line: null });
  assert.deepEqual(workspaceLink('README:5'), { path: 'README', line: 5 });
  for (const href of ['file://a-machine/share/a', 'https://example.com/a', '//example.com/a', '#heading', '/x%00', '/x%zz']) {
    assert.equal(workspaceLink(href), null);
  }
});

test('file previews read bounded content from the selected workspace and reject escapes', async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'workbench-file-preview-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const workspace = join(directory, 'workspace');
  const cwd = join(workspace, 'child');
  await mkdir(cwd, { recursive: true });
  const text = join(workspace, 'read me.md');
  const outside = join(directory, 'outside.txt');
  await writeFile(text, '# 预览\n');
  await writeFile(outside, 'external contents');
  await symlink(outside, join(cwd, 'escape'));
  await writeFile(join(cwd, 'large.txt'), Buffer.alloc(768 * 1024 + 1, 32));
  await writeFile(join(cwd, 'binary'), Buffer.from([0, 1]));
  for (const path of [text, '../read me.md']) {
    const file = await readWorkspaceFile(path, cwd, workspace);
    assert.equal(file.mime, 'text/markdown');
    assert.equal(Buffer.from(file.content, 'base64').toString(), '# 预览\n');
  }
  await assert.rejects(readWorkspaceFile(outside, cwd, workspace), { status: 403 });
  await assert.rejects(readWorkspaceFile('escape', cwd, workspace), { status: 403 });
  await assert.rejects(readWorkspaceFile('large.txt', cwd, workspace), { status: 413 });
  await assert.rejects(readWorkspaceFile('binary', cwd, workspace), { status: 415 });
  await assert.rejects(readWorkspaceFile('missing.txt', cwd, workspace), { status: 404 });
});

test('MDN documentation links map to the corresponding public Markdown source', () => {
  const mdn = 'https://developer.mozilla.org';
  const raw = 'https://raw.githubusercontent.com/mdn/';
  assert.equal(source(mdn + '/en-US/docs/Web/HTTP/Reference/Headers/X-Frame-Options#examples').endpoint,
    raw + 'content/main/files/en-us/web/http/reference/headers/x-frame-options/index.md');
  assert.equal(source(mdn + '/zh-CN/docs/Web/HTTP/Reference/Headers/X-Frame-Options').endpoint,
    raw + 'translated-content/main/files/zh-cn/web/http/reference/headers/x-frame-options/index.md');
  assert.equal(source(mdn + '/en-US/blog/'), null);
  assert.equal(source(mdn + '/en-US/docs/Web/%2fdata'), null);
});

test('Android documentation previews fetch only paths on the official mirror', () => {
  const path = '/design/ui/mobile/guides/foundations/system-bars';
  const preview = source('https://developer.android.com' + path + '?hl=en#gesture-navigation');
  assert.equal(new URL(preview.endpoint, 'http://localhost:7080').searchParams.get('path'), path);
  assert.equal(androidDocURL(path).href, 'https://developer.android.google.cn' + path + '?hl=en');
  for (const invalid of ['//example.com/secret', 'https://example.com', '/%2e%2e/secret', '/file?url=example']) {
    assert.throws(() => androidDocURL(invalid), /Invalid Android documentation path/);
  }
});
