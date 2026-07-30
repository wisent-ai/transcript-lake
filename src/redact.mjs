// Secret masker for the transcript lake (frozen interface, see the build
// contract). Every hit is replaced ENTIRELY by a [masked:<class>:<len>:<fp>]
// marker where class is token | entropy | assignment, len is the original
// hit length, and fp is a short prefix of a sha digest of the hit — nothing
// reversible and no plaintext prefix survives. Pattern sources are assembled
// from string pieces at runtime on purpose (write-gate survival rule).
// Pure string transform: no IO, deterministic, idempotent — marker bodies use
// separators that sit outside every hit alphabet, so a second pass is a no-op.
// No digit characters outside quoted strings and comments.
import { createHash } from 'node:crypto';

const N = (s) => Number(s);
const ZERO = N('0');
const ONE = N('1');
const FP_LEN = N('8');
const RUN_MIN = N('40');
const DISTINCT_MIN = N('16');
const GROUPS_MIN = N('3');

// Dense token alphabet shared by the entropy and assignment value classes.
const DENSE = '[A-Za-z' + '0-9' + '+/=_-]';
// Class (a), provider-token shape: a short lowercase prefix of two to seven
// letters, a dash, then a twenty-plus run of token characters.
const TOKEN_SRC = '\\b[a-z]{' + '2,7' + '}-[A-Za-z' + '0-9' + '_-]{' + '20' + ',}';
// Class (b), candidate dense runs of forty-plus characters; the per-hit
// diversity check below keeps prose and plain hex digests out.
const ENTROPY_SRC = DENSE + '{' + '40' + ',}';
// Class (c), assignment shape: an UPPER_CASE name, an equals sign, then a
// long token value. The whole assignment is the hit.
const ASSIGN_SRC = '\\b[A-Z][A-Z' + '0-9' + '_]{' + '2' + ',}=' + DENSE + '{' + '16' + ',}';

const HAS_LOWER = /[a-z]/;
const HAS_UPPER = /[A-Z]/;
const HAS_DIGIT = new RegExp('[' + '0-9' + ']');
const HAS_SYMBOL = /[+/=_-]/;
const GROUPS = [HAS_LOWER, HAS_UPPER, HAS_DIGIT, HAS_SYMBOL];

function fingerprint(value) {
  return createHash('sha' + '256').update(value).digest('hex').slice(ZERO, FP_LEN);
}

function marker(cls, value) {
  return '[masked:' + cls + ':' + String(value.length) + ':' + fingerprint(value) + ']';
}

// High-entropy filter: long enough, many distinct characters, and drawing on
// several character groups at once. Lowercase prose (one group) and plain hex
// digests (two groups, sixteen distinct at most) both fail this on purpose.
function denseEnough(run) {
  if (run.length < RUN_MIN) return false;
  if (new Set(run).size < DISTINCT_MIN) return false;
  let groups = ZERO;
  for (const probe of GROUPS) {
    if (probe.test(run)) groups += ONE;
  }
  return groups >= GROUPS_MIN;
}

export function createMasker() {
  const tally = { token: ZERO, entropy: ZERO, assignment: ZERO };
  const assignRe = new RegExp(ASSIGN_SRC, 'g');
  const tokenRe = new RegExp(TOKEN_SRC, 'g');
  const entropyRe = new RegExp(ENTROPY_SRC, 'g');

  function sub(text, re, cls, guard) {
    return text.replace(re, (hit) => {
      if (guard && !guard(hit)) return hit;
      tally[cls] += ONE;
      return marker(cls, hit);
    });
  }

  // Order matters: whole assignments first, then provider-shaped tokens, then
  // leftover dense runs, so each secret is attributed to its richest class.
  function mask(text) {
    if (typeof text !== 'string') return '';
    if (!text) return text;
    let out = sub(text, assignRe, 'assignment');
    out = sub(out, tokenRe, 'token');
    return sub(out, entropyRe, 'entropy', denseEnough);
  }

  return { mask, counts: () => ({ ...tally }) };
}
