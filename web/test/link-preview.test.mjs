import assert from 'node:assert/strict';
import test from 'node:test';
import { linkPreviewSource } from '../public/link-preview.js';

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
  assert.equal(source('https://github.com/owner/repository'), null);
  assert.equal(source('https://github.com:8080/owner/repository/pull/42/files'), null);
  assert.equal(source('https://secret@github.com/owner/repository/pull/42/files'), null);
  assert.equal(source('https://docs.github.com/api/article/body'), null);
  assert.equal(source('https://example.com/owner/repository/pull/42/files'), undefined);
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
