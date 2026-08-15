# business-intel — briefing template & question checklists

Load this when you want the full fill-in structure and the deeper per-lens
question banks. The SKILL.md has the loop; this has the scaffolding.

---

## Briefing skeleton (fill every section; delete nothing — write "n/a — <why>")

```
# Board Briefing — <project> — <YYYY-MM-DD> — <scope/lens>

## TL;DR
<3–5 sentences. Lead with the one decision or risk that matters most this week.>

## Scorecard
| Dimension | Value | Trend | Note |
|-----------|-------|-------|------|
| Delivery (done/pending/blocked) | | | |
| Phases complete | x / y | | |
| Velocity (chunks shipped, recent) | | ↑/→/↓ | |
| WIP (in_progress) | | | >1 = thrash |
| Open demand (signals) | | | high-confidence unbuilt? |
| Quality (gates) | green/red | | shipping-on-red? |
| Top blocker | | | on whom? |

## CEO lens
- **Market & timing:** <claim> — <grounding: serpapi/roadmap> — *so what.*
- **Competition & positioning:** <named competitors, prices, whitespace, the ownable sentence>
- **Monetization:** <model, on-roadmap?, nearest path to $1>
- **Moat:** <what compounds; tied to a real asset; skeptic's verdict>
- **Focus & opportunity cost:** <is the order value-maximizing? what to STOP>
- **Business risk:** <concentration / dependency / regulatory / attention-runway>

## CTO lens
- **Architecture health:** <claim w/ file:line or module> — *so what.*
- **Tech-debt register:** <ranked concrete items: what / where / interest / fix cost>
- **Delivery capability:** <gate strength, coverage shape, ship-on-green?>
- **Build vs buy:** <capability → hand-roll vs dependency → recommendation + reason>
- **Scaling & security risk:** <what breaks at 10×, attack surface, data exposure>
- **Key-person / maintainability:** <bus-factor-1 modules / undocumented load-bearing code>

## Decision queue (human's calls)
1. <fork> — recommendation: <x> — would change if: <fact>
2. ...

## Recommended moves (about to write to native state)
- reprioritize: <chunk> → <priority> (reason)
- backlog bet: <new chunk>
- signal: <risk/opportunity to churn>

## Honest gaps
<what couldn't be verified and why>
```

---

## CEO question bank (pick the live ones; don't run all)

**Market** — Who exactly is the buyer (role, willingness to pay)? What triggers the purchase? Is the wedge winnable by a small team? Why now (what changed in 2026)? Is demand growing, flat, or shrinking (search it)?

**Competition** — Name 3–5 real competitors. What do they charge? What do they do badly that this project does well? What's the one positioning sentence this project can defensibly own? Is there a "do nothing / spreadsheet / incumbent" competitor that's hardest to beat?

**Monetization** — One-time, subscription, usage, take-rate, lead-gen? Is the revenue path on the roadmap or perpetually "later"? What's the smallest thing that could earn the first dollar? What would a customer pay *today* for what already exists?

**Moat** — Does anything compound with usage (data, content, network, switching cost, brand, a genuinely hard technical asset)? Or is it copyable in a weekend? Tie the claimed moat to a concrete artifact in the repo.

**Focus** — If you could only ship 3 more chunks, which 3? What on the roadmap is value-destroying or vanity? What should be killed or deferred to fund the winners?

**Risk** — Single buyer / channel / dependency? Regulatory or platform risk? Founder-attention runway (the scarcest resource in a solo/small project)?

---

## CTO question bank (ground in ministr first)

**Architecture** — Does the module/boundary structure match the stated ambition? Where's the coupling that will hurt at 10×? Is the data model right or already fighting the domain? Cite `file:line`/module, never adjectives.

**Debt** — List concrete items (file/module), the interest each accrues (slower changes, bugs, onboarding friction), and the fix cost. Rank by leverage (interest ÷ fix-cost). Distinguish *deliberate* debt (fine) from *accidental* (dangerous).

**Delivery** — What's the real gate (lint/type/test/e2e)? Does it run on every change? Does the team ship on green or override red? Coverage shape — are the load-bearing paths tested or just the easy ones? A strong gate is a balance-sheet asset; score it.

**Build vs buy** — For each major capability on/near the roadmap (auth, payments, search, email, CMS, analytics…): is hand-rolling a moat or a distraction? What's the boring proven dependency (search it)? Recommend with the reason and the switching cost.

**Scaling & security** — What's the first thing that breaks under load? What's the attack surface and the data/privacy exposure? Is there a secret-management / tenant-isolation / input-validation gap? File real risks as `signal_capture`; don't fix here.

**Maintainability / bus factor** — What does exactly one person understand? What load-bearing module has no tests and no docs? What would a new contributor trip on in week one?

---

## Mapping findings → native mutations (the /roadmap integration)

| Finding type | Native action | Status |
|---|---|---|
| "Chunk X is higher value than its order" | `roadmap_reprioritize(X, p, reason)` | proposal (human decides) |
| "We should also build Y" (opportunity/bet) | `roadmap_add_chunk(Y, status:'backlog')` | backlog (not pending) |
| "Z is a risk needing research" | `signal_capture(Z)` | new signal → `/signals` |
| "Direction A may be wrong, needs study" | `signal_capture` or recommend `/roadmap-refresh A` | — |
| The reasoning behind any of the above | `roadmap_link(id, 'think:N')` + `roadmap_record_refresh` | provenance |

Never: reorder priorities directly, promote backlog→pending, mark chunks done,
or write feature code. Those are the human's call or `/roadmap`'s job.
