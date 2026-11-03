#!/usr/bin/env bash
# Commit and push to the GitHub repo.
set -e
cd "$(dirname "$0")"
if ! git rev-parse --git-dir >/dev/null 2>&1; then
  git init
  git remote add origin https://github.com/StarAtNyte/seshat.git
  git branch -M main
fi
git add -A
if git diff --cached --quiet; then
  echo "Nothing to sync."
  exit 0
fi
git commit -m "sync: $(date -u '+%Y-%m-%d %H:%M UTC')" || true
git push -u origin main
