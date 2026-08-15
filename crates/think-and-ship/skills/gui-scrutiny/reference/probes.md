# gui-scrutiny — mechanical probe library

Reusable `browser_evaluate` recipes for Mode 2.4 (exploratory mechanical scrutiny).
Each returns a small JSON verdict you can assert on. Adapt selectors to the project.
Loaded on demand by `/gui-scrutiny` — don't inline these into the SKILL.md.

> A probe that throws is a **probe** bug (wrong selector / prototype), not an app
> regression — fix the probe and re-run.

## Zero console errors

```js
// after: browser_console_messages({ onlyErrors: true })
// assert: 0 errors. favicon 404 is the usual harmless exception — call it out.
```

## Rendered richly (not an empty shell)

```js
() => ({
  rows: document.querySelectorAll('[data-roving-item], [role="row"], .card').length,
  nonEmpty: document.body.innerText.trim().length > 50,
})
// assert: the expected count, not a shell.
```

## Interactivity survives (handlers + state work)

```js
() => {
  const tab = [...document.querySelectorAll('[role="tab"]')].find(t => /bridges/i.test(t.textContent));
  tab?.click();
  return new Promise(r => setTimeout(() => r({
    clicked: !!tab,
    consequence: !!document.querySelector('header span'),  // a subtitle/state changed
  }), 300));
}
```

## Keyboard nav — assert activeElement moves

```js
() => {
  const first = document.querySelector('[data-roving-item]');
  first?.focus();
  first?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  const items = [...document.querySelectorAll('[data-roving-item]')];
  return { movedTo: items.indexOf(document.activeElement) }; // expect 1, not 0
}
```

## No redundant work — invoke/command counter

```js
// Wrap the app's command entry, interact, then read the count.
() => {
  const inv = window.__TAURI_INTERNALS__.invoke;
  window.__probeCount = 0;
  window.__TAURI_INTERNALS__.invoke = (...a) => { window.__probeCount++; return inv(...a); };
  return 'wrapped';
}
// …switch a lens / re-open a cached view…
() => ({ invokes: window.__probeCount })  // assert: a cache hit doesn't re-invoke
```

## Navigation conventions / no dead ends

```js
() => ({
  open:    document.querySelectorAll('button[title^="Open"]').length,
  inspect: document.querySelectorAll('[role="button"][title^="Inspect"]').length,
})
// assert: every row has the intended action; none is a dead end.
```

## Controlled input round-trips (state holds)

```js
() => {
  const el = document.querySelector('textarea, input');
  if (!el) return { hasInput: false };
  const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, 'probe value');
  el.dispatchEvent(new Event('input', { bubbles: true }));
  return { held: el.value === 'probe value' };  // proves controlled state round-trips
}
```

## Persisted state after a transient effect (flash fades, marker stays)

```js
() => new Promise(r => setTimeout(() => r({
  persisted: !!document.querySelector('.code-line-active'),   // durable marker stays
  transientGone: !document.querySelector('.code-line-focus'), // flash removed
}), 2200))  // wait past the transient's timeout
```

## Typography / computed-style audit (read the type, don't eyeball it)

The mechanical complement to the §2.3 component-crop: small type is unreadable on a
full-frame shot, so *measure* it. Assert against INTENT, not just "exists".

```js
() => {
  const pick = (sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    const cs = getComputedStyle(el);
    return {
      text: el.textContent.trim().slice(0, 24),
      px: parseFloat(cs.fontSize),
      weight: cs.fontWeight,
      family: cs.fontFamily.split(',')[0].replace(/["']/g, ''),
      transform: cs.textTransform,        // 'uppercase' on letter-keys/identifiers is a smell
      tracking: cs.letterSpacing,
    };
  };
  // list the small-type nodes you care about — adapt selectors
  return { kbd: pick('kbd'), badge: pick('[class*="badge"]'), label: pick('[class*="label"]') };
}
// assert: px >= 11 for any legible hint (not just a faint caption); transform !== 'uppercase'
// on letter-key / identifier text; family matches the system's kbd/badge grammar (find it
// via ministr — don't invent a one-off). Mismatch ⇒ fix the component, re-crop, re-read.
```

## forced-colors / Windows High Contrast Mode survival

```
// Emulate via Playwright forcedColors:'active' (or CDP Emulation.setEmulatedMedia),
// then assert custom controls keep a visible boundary + focus:
() => {
  const btn = document.querySelector('[role="button"]');
  const cs = getComputedStyle(btn);
  return { hasBorder: cs.borderStyle !== 'none' || cs.outlineStyle !== 'none' };
}
```

## Anti-slop probes (§0 prime directive, mechanical arm)

The slop tells a machine *can* see. Run these on every audited view; each returns a
verdict that should be **empty/zero** on a clean UI. Pair with the §2.3 designer read for
the tells a machine can't see (taste, hierarchy, generic-ness).

### Emoji-as-UI scan (near-always slop)

```js
() => {
  const rx = /\p{Extended_Pictographic}/u;
  const hits = [];
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walk.nextNode(); n; n = walk.nextNode()) {
    const t = n.textContent.trim();
    if (t && rx.test(t)) hits.push(t.slice(0, 40));
  }
  // also catch emoji hidden in aria-labels / titles / alt
  document.querySelectorAll('[aria-label],[title],img[alt]').forEach(el => {
    const v = el.getAttribute('aria-label') || el.getAttribute('title') || el.getAttribute('alt') || '';
    if (rx.test(v)) hits.push('attr:' + v.slice(0, 40));
  });
  return { emojiCount: hits.length, hits };  // assert: 0
}
```

### Non-token color / off-scale spacing census

```js
() => {
  // Tokens resolve via CSS custom properties; raw literals in computed style that
  // aren't on the scale are the "one-off value" slop tell. Flag suspicious literals.
  const okSpace = new Set(['0px','4px','8px','12px','16px','24px','32px','48px','64px']); // adapt to the scale
  const offSpace = new Set(), rawColor = new Set();
  document.querySelectorAll('*').forEach(el => {
    const cs = getComputedStyle(el);
    [cs.padding, cs.margin, cs.gap].forEach(v => v.split(' ').forEach(p => {
      if (/^\d+px$/.test(p) && p !== '0px' && !okSpace.has(p)) offSpace.add(p);
    }));
  });
  return { offScaleSpacing: [...offSpace], offScaleCount: offSpace.size };
  // assert: offScaleCount 0 (or only justified exceptions). Complements the source-grep
  // token-purity gate (§1) — this catches values that survive to the rendered DOM.
}
```

### Gradient + shadow census (unmotivated surface slop)

```js
() => {
  let gradients = 0, shadows = 0;
  const examples = [];
  document.querySelectorAll('*').forEach(el => {
    const cs = getComputedStyle(el);
    if (/gradient/i.test(cs.backgroundImage)) { gradients++; if (examples.length < 5) examples.push(cs.backgroundImage.slice(0, 60)); }
    if (cs.boxShadow && cs.boxShadow !== 'none') shadows++;
  });
  return { gradients, shadows, examples };
  // assert against INTENT: a utility view with many gradients, or a shadow on nearly
  // every element, is the "shadow/gradient everything" tell. Judge vs the design system.
}
```

### Placeholder / fake-data / marketing-copy scan

```js
() => {
  const rx = /lorem|ipsum|placeholder|john doe|jane doe|example\.com|foo ?bar|click here|lorem ipsum|✨|🚀|welcome to your/i;
  const hits = [];
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walk.nextNode(); n; n = walk.nextNode()) {
    const t = n.textContent.trim();
    if (t.length > 1 && rx.test(t)) hits.push(t.slice(0, 60));
  }
  return { slopCopy: hits.length, hits };  // assert: 0
}
```

### Accessibility-honesty scan (a11y theater)

```js
() => {
  // click-divs masquerading as buttons
  const clickDivs = [...document.querySelectorAll('div,span')].filter(el =>
    el.onclick || el.getAttribute('onclick')).map(el => el.className).slice(0, 10);
  // aria-labels that merely echo the visible text (label adds nothing / lies)
  const echoes = [];
  document.querySelectorAll('[aria-label]').forEach(el => {
    const al = el.getAttribute('aria-label').trim().toLowerCase();
    const vis = (el.textContent || '').trim().toLowerCase();
    if (vis && al === vis) echoes.push(vis.slice(0, 40));
  });
  return { clickDivs, ariaEchoes: echoes };  // assert: both empty
}
```

## Object-model fidelity probes (§2.5 OOUX — does the UI honor its objects?)

Verify the object model, not just the surface (`/craft` §B0/§A6; OOUX manifesto #9). These
assume components tag themselves with `data-object="<name>"` and `data-renderer="<component>"`
(+ `data-zoom` for the appearance) — adapt to the project's own markers / testids.

### One renderer per object (no drifting second renderer)

```js
() => {
  const byObject = {};
  document.querySelectorAll('[data-object]').forEach(el => {
    const obj = el.getAttribute('data-object');
    const renderer = el.getAttribute('data-renderer')
      || el.tagName.toLowerCase() + '.' + (el.className || '').split(' ')[0];
    (byObject[obj] ??= new Set()).add(renderer);
  });
  // zoom (chip/row/card/detail) is a PROP of one component, not a new component
  return Object.fromEntries(Object.entries(byObject).map(([o, s]) => [o, [...s]]));
  // assert: each object maps to ONE renderer family. Two+ ⇒ a drifting/dead renderer (§A6).
}
```

### CTA-verb consistency per object (one verb, one name)

```js
() => {
  const verbs = {};
  document.querySelectorAll('[data-object]').forEach(card => {
    const obj = card.getAttribute('data-object');
    (verbs[obj] ??= new Set());
    card.querySelectorAll('button, [role="button"], a[role="button"]').forEach(b =>
      verbs[obj].add((b.getAttribute('aria-label') || b.textContent).trim().toLowerCase()));
  });
  return Object.fromEntries(Object.entries(verbs).map(([o, s]) => [o, [...s]]));
  // assert: the same object's CTA labels are consistent across its appearances —
  // 'sign' vs 'add signature' for one object is one CTA with two names (a finding).
}
```

### Relationships render through shared children (nesting, not re-implementation)

```js
() => {
  const parent = document.querySelector('[data-object="plan"]');         // adapt
  if (!parent) return { parentFound: false };
  const ref = document.querySelector('[data-object="action"][data-zoom="row"]')
    ?.getAttribute('data-renderer');
  const children = [...parent.querySelectorAll('[data-object="action"]')]; // adapt
  return {
    parentFound: true,
    nestedCount: children.length,
    sharesChildRenderer: children.length > 0
      && children.every(c => c.getAttribute('data-renderer') === ref),
  };
  // assert: nestedCount > 0 AND sharesChildRenderer — the parent didn't re-implement the
  // child's look; cardinality (a list of children) is visible.
}
```

## Notes

- Prefer returning a small object over logging — you assert on the return value.
- For async renderers, **poll** for a ready selector before probing (don't fixed-sleep).
- Keep probes selector-light and project-specific; these are templates, not contracts.
- The anti-slop probes catch the **mechanical** tells only. Taste, hierarchy, and
  "is this generic?" stay a human-grade read (§2.3) — the probes narrow the surface, they
  don't replace the judgment.
