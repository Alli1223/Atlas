#!/usr/bin/env node
/**
 * Writes src/styles/tokens.css from scripts/tokens.mjs.
 *
 * The output is committed, so nothing needs to run this at install/build time —
 * it exists so the light and dark theme blocks are emitted from one source object
 * instead of being hand-maintained in three places (which guarantees drift).
 *
 * Run after editing scripts/tokens.mjs:  npm run gen:tokens
 */
import { writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { renderTokensCss } from './tokens.mjs'

const out = resolve(dirname(fileURLToPath(import.meta.url)), '../src/styles/tokens.css')
writeFileSync(out, renderTokensCss(), 'utf8')
console.log(`wrote ${out}`)
