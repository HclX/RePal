/**
 * Lightweight, zero-dependency YAML subset parser for PalRemake static browser runtime.
 * Supports:
 * - `#` full-line and inline comments
 * - Nested mappings (`key: value` & nested indentation)
 * - Block sequences (`- item`, `- key: val`)
 * - Literal block scalars (`|` and `|-`) for multi-line Chinese dialogue
 * - Inline flow arrays (`[a, b, c]`) and flow objects (`{a: 1, b: 2}`)
 * - Quoted strings, numbers, booleans, null
 */

const isSafeKey = (k) => k !== "__proto__" && k !== "constructor" && k !== "prototype";

function isEscaped(str, index) {
  let slashes = 0;
  for (let i = index - 1; i >= 0 && str[i] === "\\"; i--) slashes++;
  return (slashes & 1) === 1;
}

function stripInlineComment(str) {
  let inSingle = false;
  let inDouble = false;
  for (let i = 0; i < str.length; i++) {
    const ch = str[i];
    if (ch === "'" && !inDouble) {
      if (inSingle && str[i + 1] === "'") {
        i++;
        continue;
      }
      inSingle = !inSingle;
    } else if (ch === '"' && !inSingle) {
      if (!isEscaped(str, i)) {
        inDouble = !inDouble;
      }
    } else if (ch === "#" && !inSingle && !inDouble) {
      if (i === 0 || str[i - 1] === " " || str[i - 1] === "\t") {
        return str.slice(0, i).trimEnd();
      }
    }
  }
  return str.trimEnd();
}

function splitTopLevelCommas(inner) {
  const parts = [];
  let cur = "";
  let depth = 0;
  let inS = false;
  let inD = false;
  for (let i = 0; i < inner.length; i++) {
    const c = inner[i];
    if (c === "'" && !inD) inS = !inS;
    else if (c === '"' && !inS && !isEscaped(inner, i)) inD = !inD;
    else if (!inS && !inD) {
      if (c === "[" || c === "{") depth++;
      else if (c === "]" || c === "}") depth--;
      else if (c === "," && depth === 0) {
        parts.push(cur.trim());
        cur = "";
        continue;
      }
    }
    cur += c;
  }
  if (cur.trim().length > 0) parts.push(cur.trim());
  return parts;
}

function parseScalar(raw) {
  const s = raw.trim();
  if (s === "" || s === "null" || s === "~") return null;
  if (s === "true" || s === "True") return true;
  if (s === "false" || s === "False") return false;
  if (s.startsWith('"') && s.endsWith('"') && s.length >= 2) {
    try {
      return JSON.parse(s);
    } catch {
      return s.slice(1, -1);
    }
  }
  if (s.startsWith("'") && s.endsWith("'") && s.length >= 2) {
    return s.slice(1, -1).replace(/''/g, "'");
  }
  if (s.startsWith("[") && s.endsWith("]")) {
    const inner = s.slice(1, -1).trim();
    if (!inner) return [];
    return splitTopLevelCommas(inner).map(parseScalar);
  }
  if (s.startsWith("{") && s.endsWith("}")) {
    const inner = s.slice(1, -1).trim();
    const obj = {};
    if (!inner) return obj;
    for (const entry of splitTopLevelCommas(inner)) {
      const idx = findColonIndex(entry);
      if (idx !== -1) {
        const k = String(parseScalar(entry.slice(0, idx)));
        if (isSafeKey(k)) obj[k] = parseScalar(entry.slice(idx + 1));
      }
    }
    return obj;
  }
  if (/^-?(0|[1-9]\d*)(\.\d+)?$/.test(s)) {
    const n = Number(s);
    if (Number.isSafeInteger(n) || !Number.isNaN(n)) return n;
  }
  return s;
}

function findColonIndex(line) {
  let inS = false;
  let inD = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === "'" && !inD) inS = !inS;
    else if (c === '"' && !inS && !isEscaped(line, i)) inD = !inD;
    else if (c === ":" && !inS && !inD) {
      if (i + 1 === line.length || line[i + 1] === " " || line[i + 1] === "\t") {
        return i;
      }
    }
  }
  return -1;
}

export function parseYaml(text) {
  const rawLines = text.replace(/\r\n/g, "\n").split("\n");
  const countIndent = (ln) => {
    let n = 0;
    while (n < ln.length && ln[n] === " ") n++;
    return n;
  };

  let idx = 0;
  const total = rawLines.length;

  function skipIgnorable() {
    while (idx < total) {
      const trimmed = rawLines[idx].trim();
      if (trimmed === "" || trimmed.startsWith("#") || trimmed === "---") {
        idx++;
      } else {
        break;
      }
    }
  }

  function readBlockScalar(parentIndent, chompStrip, folded) {
    const collected = [];
    let blockIndent = -1;
    while (idx < total) {
      const raw = rawLines[idx];
      if (raw.trim() === "") {
        collected.push("");
        idx++;
        continue;
      }
      const ind = countIndent(raw);
      if (blockIndent === -1) {
        if (ind <= parentIndent) break;
        blockIndent = ind;
      }
      if (ind < blockIndent) break;
      collected.push(raw.slice(blockIndent));
      idx++;
    }
    while (collected.length > 0 && collected[collected.length - 1] === "") {
      collected.pop();
    }
    const joined = folded ? collected.join(" ") : collected.join("\n");
    return chompStrip ? joined : joined + (collected.length ? "\n" : "");
  }

  function parseNode(minIndent) {
    skipIgnorable();
    if (idx >= total) return null;
    const line = rawLines[idx];
    const ind = countIndent(line);
    if (ind < minIndent) return null;
    const content = stripInlineComment(line.slice(ind));
    if (content === "-" || content.startsWith("- ")) {
      return parseSequence(ind);
    }
    return parseMapping(ind);
  }

  function parseSequence(seqIndent) {
    const arr = [];
    while (idx < total) {
      skipIgnorable();
      if (idx >= total) break;
      const line = rawLines[idx];
      const ind = countIndent(line);
      if (ind !== seqIndent) break;
      const content = stripInlineComment(line.slice(ind));
      if (!(content === "-" || content.startsWith("- "))) break;

      const rest = content.slice(1).trim();
      idx++;
      if (rest === "") {
        arr.push(parseNode(seqIndent + 1));
      } else if (rest === "|" || rest === "|-" || rest === ">" || rest === ">-") {
        arr.push(readBlockScalar(seqIndent, rest.endsWith("-"), rest.startsWith(">")));
      } else {
        const colonIdx = findColonIndex(rest);
        if (colonIdx !== -1) {
          // Inline mapping item `- op: say` followed by additional indented keys!
          const itemObj = {};
          const k0 = String(parseScalar(rest.slice(0, colonIdx)));
          const v0Raw = rest.slice(colonIdx + 1).trim();
          const itemKeyIndent = ind + 2;
          const isSafeKey = (k) => k !== "__proto__" && k !== "constructor" && k !== "prototype";
          let v0;
          if (v0Raw === "|" || v0Raw === "|-" || v0Raw === ">" || v0Raw === ">-") {
            v0 = readBlockScalar(itemKeyIndent, v0Raw.endsWith("-"), v0Raw.startsWith(">"));
          } else if (v0Raw === "") {
            v0 = parseNode(itemKeyIndent + 1);
          } else {
            v0 = parseScalar(v0Raw);
          }
          if (isSafeKey(k0)) itemObj[k0] = v0;
          // Merge subsequent properties indented >= itemKeyIndent
          skipIgnorable();
          if (idx < total && countIndent(rawLines[idx]) >= itemKeyIndent) {
            const subIndent = countIndent(rawLines[idx]);
            const subContent = stripInlineComment(rawLines[idx].slice(subIndent));
            if (!(subContent === "-" || subContent.startsWith("- "))) {
              const extra = parseMapping(subIndent);
              if (extra && typeof extra === "object") {
                for (const [ek, ev] of Object.entries(extra)) {
                  if (isSafeKey(ek)) itemObj[ek] = ev;
                }
              }
            }
          }
          arr.push(itemObj);
        } else {
          arr.push(parseScalar(rest));
        }
      }
    }
    return arr;
  }

  function parseMapping(mapIndent) {
    const obj = {};
    const isSafeKey = (k) => k !== "__proto__" && k !== "constructor" && k !== "prototype";
    while (idx < total) {
      skipIgnorable();
      if (idx >= total) break;
      const line = rawLines[idx];
      const ind = countIndent(line);
      if (ind !== mapIndent) break;
      const content = stripInlineComment(line.slice(ind));
      if (content === "-" || content.startsWith("- ")) break;
      const colonIdx = findColonIndex(content);
      if (colonIdx === -1) {
        idx++;
        continue;
      }
      const key = String(parseScalar(content.slice(0, colonIdx)));
      const valRaw = content.slice(colonIdx + 1).trim();
      idx++;
      let val;
      if (valRaw === "|" || valRaw === "|-" || valRaw === ">" || valRaw === ">-") {
        val = readBlockScalar(mapIndent, valRaw.endsWith("-"), valRaw.startsWith(">"));
      } else if (valRaw === "") {
        skipIgnorable();
        if (idx < total && countIndent(rawLines[idx]) >= mapIndent) {
          // Note: PyYAML indents sequence items under a mapping key at EITHER `mapIndent` (`  s_105a:\n  - op: ...`)
          // OR `mapIndent + 2`! Handle `countIndent == mapIndent` when next line starts with `- `!
          const nextInd = countIndent(rawLines[idx]);
          const nextTrim = stripInlineComment(rawLines[idx].slice(nextInd));
          if (nextInd > mapIndent) {
            val = parseNode(nextInd);
          } else if (nextInd === mapIndent && (nextTrim === "-" || nextTrim.startsWith("- "))) {
            val = parseSequence(nextInd);
          } else {
            val = null;
          }
        } else {
          val = null;
        }
      } else {
        val = parseScalar(valRaw);
      }
      if (isSafeKey(key)) obj[key] = val;
    }
    return obj;
  }

  return parseNode(0) ?? {};
}
