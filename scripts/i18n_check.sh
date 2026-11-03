#!/bin/sh
# Checks assets/i18n/*.json against the strings in src/ and lists what is
# still untranslated per language. Fails on stale keys or placeholder drift.
cd "$(dirname "$0")/.." && cargo test i18n::tests::catalogs_match_sources -- --nocapture
