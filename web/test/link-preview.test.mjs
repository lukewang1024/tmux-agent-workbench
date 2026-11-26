import assert from 'node:assert/strict';
import test from 'node:test';
import { githubPreviewSource } from '../public/link-preview.js';

test('GitHub links resolve only to the public preview endpoints', () => {
  const source = (url) => githubPreviewSource(new URL(url));
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
