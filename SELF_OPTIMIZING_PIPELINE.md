# Self-Optimizing Planning Pipeline — Design Document

## Prior Art

Epic `game1-cght` (closed) implemented three components:
1. **Local log parser** — reads explicitly enabled `var/stepwise_logs/*/channels/*.jsonl` workflow checkpoints
2. **PRIM rules** — extracts IF-THEN rules predicting correction likelihood from plan patterns
3. **CMA-ES optimizer** — auto-tunes profile parameters (exploreCount, branchingThreshold, etc.) per model

All three were Python, operating on the TypeScript server's JSONL output. The Rust port preserves the same JSONL format.

## Current State (v0.2)

```
Step In → Route(room, channel) → Validate → Track Usage → Log JSONL → Response Out
                                              ↓                ↓
                     var/stepwise_logs/{room}/     DECIDE() decision tree
                     channels/{channel}.jsonl     (compressed, profile-adaptive)
```

Branching + merge step structure. 7 model profiles (Gemini Flash/Pro/catch-all,
DeepSeek, Grok, Claude, Llama/Nemotron). Compressed decision-tree guidance (~120 tokens).
Each room/channel has isolated history and heuristics. No feedback loop yet.

See CONTRIBUTING.txt for the adaptive guidance loading roadmap (Phase 2-4).

## Proposed Upgrades

### Upgrade 1: Feedback Loop (Close the Learning Cycle)

**Problem**: Profiles are hand-tuned. CMA-ES optimizer exists in Python but profiles never actually update.

**Design**: The Rust server reads profiles from a JSON file at startup (already does this). The learning pipeline writes updated profiles after analysis. The server picks them up on next session.

```
Session N:
  kp-stepwise → JSONL logs
                                ↓
Between sessions:
  local analyzer reads opted-in logs
  correlates with session outcomes (corrections, acceptances)
  PRIM extracts rules
  CMA-ES optimizes profile params
                                ↓
  etc/stepwise_profiles.json (updated)
                                ↓
Session N+1:
  kp-stepwise reads updated profiles
```

**What changes in the Rust server**: Nothing for the basic loop — the JSONL output format is already compatible. For advanced integration:
- Add a `reload_profiles` MCP tool that hot-reloads profiles without restart
- Add a `get_compliance_stats` MCP tool that returns session-level metrics for the optimizer
- Add outcome correlation fields to JSONL: `session_id`, `step_hash` for join keys

**What stays in Python**: The scavenger parser, PRIM extraction, and CMA-ES optimization. These are batch analytics — no reason to port to Rust.

### Upgrade 2: DAG Step Structures

**Problem**: Current model is linear chain + branches. Branches are parallel alternatives, but you can't express "step C depends on both step A and step B" or "merge insights from branches X and Y into a single conclusion."

**Current structure**:
```
T1 → T2 → T3 → T4
      ↓
      T2b (branch) → T2c
```

**Proposed structure** — directed acyclic graph:
```
T1 → T2 ──→ T4 (depends on T2 + T3)
 ↓         ↗
 T3 ──────
 ↓
 T3b (branch) → T3c
                  ↓
                  T5 (merge: T4 + T3c)
```

**New fields on StepData**:
```rust
/// Steps this step depends on (must all exist before this one)
pub depends_on: Option<Vec<u32>>,  // step numbers

/// Merge mode: combine insights from specified steps
pub merge_from: Option<Vec<u32>>,  // step numbers to synthesize
```

**Validation changes**:
- `depends_on` steps must exist in history
- Cycle detection (no step can depend on itself or a descendant)
- `merge_from` triggers a synthesized context window containing only the specified steps

**Compliance changes**:
- Track graph diameter (longest path) vs step count (detect unnecessary linear chains)
- Track fan-out (how many steps branch from a single point)
- Warn on "star topology" (everything depends on T1 — no intermediate reasoning)

**Response changes**:
- Include `dependency_graph` in response (adjacency list)
- Include `ready_to_merge` — steps whose dependencies are all satisfied
- Include `orphaned_branches` — branches that were never merged back

**Backward compatibility**: `depends_on` and `merge_from` are optional. Without them, behavior is identical to linear+branching. The DAG is implicit when these fields are used.

### Upgrade 3: Outcome-Based Optimization (ExoPrior I1)

**Problem**: Self-reported confidence is unreliable (ExoPrior I1: "self-assessment unreliable"). A model saying "0.9 confidence" doesn't mean it's right.

**Current**: Compliance tracking uses confidence as the primary signal. Branch when confidence < 0.6. Exit when confidence > 0.75.

**Proposed**: Confidence remains as a process signal (agents use it to decide when to branch), but the LEARNING system ignores confidence and uses outcome signals instead:

**Outcome signals** (external ground truth):
- Correction density: user corrections per N turns after planning completes
- Acceptance rate: output used without modification
- Re-prompting rate: user rephrased the request
- Abandonment rate: session ended without completion

**Process signals** (what the optimizer tunes):
- `branch_rate` when confidence < threshold
- `explore_count` on decision points
- Layer progression (L1→L2→L3 coverage)
- Search integration frequency
- DAG diameter vs step count

**How it works**:
1. Planning server logs process metrics in JSONL (already does this)
2. An optional local outcome adapter records coarse workflow outcomes; it does not copy conversation text or non-checkpoint model state
3. The analyzer joins on `(session_id, room_id, channel_id)`: checkpoint patterns → opted-in outcome summaries within N turns
4. PRIM extracts rules: "IF branch_rate < 0.1 AND explore_count = 1 THEN correction_rate > 0.4"
5. CMA-ES optimizes profile params to minimize correction_rate while keeping step_count reasonable

**What changes in the Rust server**:
- Add `session_id` field to every JSONL log entry (already present)
- Add `plan_complete` event when `continuationMode = "done"` with aggregate stats
- Add optional `outcome_feedback` tool: external system can report outcomes back
  ```
  outcome_feedback(session_id, correction_count, acceptance_rate)
  ```
  This enables real-time profile adjustment within a session (not just between sessions)

**Goodhart mitigations** (from the epic):
- Multi-metric Pareto optimization (not single metric)
- External ground truth (user corrections can't be gamed)
- Relative improvement (compare to baseline, not absolute)
- Hold-out evaluation (train 80%, test 20%)

## Implementation Phases

### Phase 1: Plumbing (Rust changes, no optimizer)
- Add `reload_profiles` tool
- Add `get_compliance_stats` tool
- Add `plan_complete` JSONL event with aggregate stats
- Add `depends_on` and `merge_from` fields to StepData (DAG support)
- Add cycle detection and dependency validation
- Update compliance tracking for DAG metrics

### Phase 2: Scavenger Integration (Python, batch)
- Update scavenger parser for new JSONL fields
- Add session_id join with explicit local outcome summaries
- Implement correction density calculation
- Store process→outcome correlations

### Phase 3: PRIM Rules (Python, batch)
- Extract plan→outcome rules from correlated data
- Generate human-readable rules: "IF X THEN Y"
- Write rules to `etc/plan_rules.json`
- Rust server reads rules at startup, uses them for enhanced compliance warnings

### Phase 4: CMA-ES Optimization (Python, batch)
- Multi-objective optimization of profile params
- Pareto frontier: minimize corrections, minimize step count, maximize acceptance
- Write optimized profiles to `etc/stepwise_profiles.json`
- A/B test: 10% of sessions use new profiles

### Phase 5: Real-Time Feedback (Rust)
- Add `outcome_feedback` tool
- Bayesian update of profile params within a session
- Warm-start from last optimized profiles

## Non-Goals

- **Not replacing the Python pipeline**: Batch analytics stays in Python. The Rust server is the data producer and profile consumer.
- **Not building a full ML system**: PRIM + CMA-ES is the right level of sophistication. No neural nets, no gradient descent.
- **Local workflow scope only**: analysis is per-workflow and limited to external checkpoint structure plus user-authorized outcome summaries. There is no cross-user corpus or substitute-model training.
- **Not requiring full-content logs**: metadata mode is sufficient for structural metrics; full content is an explicit local choice.
- **Not BMAD**: The planning pipeline is domain-agnostic. BMAD's role-based personas are a separate concern.

## Dependencies

- Scavenger parser (game1-cght.1, closed — needs update for new fields)
- PRIM adapter (game1-cght.2, closed — works as-is)
- CMA-ES optimizer (game1-cght.3, closed — needs update for DAG metrics)
- Claude Code session archives (already logged)
