# Reference for the AuRa implementation

A walk-through of what the AuRa branch adds on top of `master`, intended for someone reviewing the diff. Reads top-down: protocol primer first, then how each piece is implemented in code, with line-level pointers and external references.

If you only have 5 minutes: read **§1**, skim the table of files in **§2**, then jump to **§4** (Nethermind/geth references).

---

## 1. AuRa in a paragraph

AuRa (Authority Round) is the proof-of-authority consensus Gnosis ran from genesis until the merge at block 25,349,537. Time is split into 5-second **steps**. A round-robin **validator set** takes turns proposing blocks: `proposer = validators[step % len(validators)]`. The proposer signs the **bare hash** of the block header (everything except `aura_step` and `aura_seal`) with secp256k1; the 65-byte signature is stored in `aura_seal`. **Difficulty** is a score, not work: `(2^128 − 1) + parent_step − current_step`. Sequential blocks (no skips) yield difficulty `U128_MAX − 1`. There are no ommers.

The validator set evolves in three stages:
1. **List** (block 0–1300 on Gnosis): static array of addresses in genesis JSON.
2. **SafeContract** (block 1300–9,186,425 on Gnosis): set lives in a smart contract; resolved by calling `getValidators()` (selector `0xb7ab4db5`). Changes are signaled by an `InitiateChange(bytes32 parentHash, address[] newSet)` event from the validator contract; activated by a system call to `finalizeChange()` (selector `0x75286211`) at the right moment. Pre-POSDAO, "right moment" is N+1 (block right after the event). This phase is essentially "trust the contract."
3. **Contract / POSDAO** (block 9,186,425+ on Gnosis): same contract API as SafeContract, but `finalizeChange()` is gated on **rolling finality**: a block becomes finalized when more than half of unique validators have signed any blocks after it. Only then does `InitiateChange` "take effect." This is the geth `RollingFinality` rule: `sign_count.len() * 2 > validators.len()`.

Two Gnosis-specific quirks matter for execution (not consensus):
- **Block rewards**: every block (pre- AND post-merge) calls a Gnosis-specific reward contract via system call. The contract returns `(receivers, amounts)` and we credit each receiver. Goes through the AuRa `block_reward_contract_transitions` — different addresses are active at different block ranges.
- **EIP-158 disabled for AuRa system calls**: Nethermind's `SystemTransactionProcessor` keeps the empty `SYSTEM_ADDRESS` (caller of every system call) in state instead of pruning it post-Spurious-Dragon. We must do the same or state roots diverge.

---

## 2. Implementation map

The whole AuRa addition is `src/aura/` (consensus + algorithms) plus surgical changes elsewhere (chain spec parsing, EVM config, block executor, network status). New files first, then modifications:

### New files in `src/aura/`

| File | Purpose | Key entry points |
|---|---|---|
| `aura/mod.rs` | Top-level `GnosisConsensus` — wraps `EthBeaconConsensus` and dispatches AuRa vs PoS based on `header.is_pre_merge()` | `GnosisConsensus::new`, `validate_header*`, `validate_block_pre_execution`, `validate_block_post_execution` |
| `aura/seal.rs` | Seal hash + signature recovery + difficulty | `compute_seal_hash`, `recover_seal_author`, `calculate_aura_difficulty` |
| `aura/validators.rs` | Validator set wrapper with block-keyed transitions | `ValidatorSet::kind_at`, `try_get_list_validators`, `contract_address_at`, `expected_proposer` |
| `aura/finality.rs` | Rolling finality tracker (geth-compatible), with disk persistence | `RollingFinality::push` / `set_immediate_finalize` / `add_pending_transition` / `take_finalize_change` |
| `aura/config.rs` | JSON parser for the `aura` section of genesis | `AuraConfig::from_json_value` |

### Modified files

| File | Why it changed |
|---|---|
| `src/spec/gnosis_spec.rs` | Parses `aura` from genesis JSON; sets `Paris.activation_block_number` to known merge block (25,349,537 / 680,930) while keeping `fork_block: None` so the fork ID stays compatible with other Gnosis clients |
| `src/lib.rs` | Builds `GnosisConsensus` instead of `EthBeaconConsensus`; passes datadir to `GnosisEvmConfig::new_with_datadir` so rolling-finality state is persisted across restarts |
| `src/cli/gnosis_cli.rs` | Same swap for the CLI helper components |
| `src/main.rs` | Removes pre-merge state import (the AuRa branch syncs from genesis); still keeps `DefaultStorageValues::default().with_v2(false).try_init()` |
| `src/evm_config.rs` | `gnosis_revm_spec` (correct `SpecId` for pre-merge headers); pre-merge `disable_base_fee`; Constantinople EIP-1283 SSTORE gas overrides; `GnosisBlockExecutionCtx` is built here per block, including `compute_finalize_change_address` (list→contract transition logic), `validator_contract`, `block_rewards_override`, `aura_bytecode_rewrites`, and the shared `Arc<Mutex<RollingFinality>>` |
| `src/block.rs` | `GnosisBlockExecutionCtx` carries the AuRa fields; `apply_pre_execution_changes` runs AuRa system calls (validator init, `finalizeChange`, refresh); `finish` detects `InitiateChange` events from receipts + reward logs and feeds the rolling-finality tracker; helpers `aura_system_call_and_commit` and `refresh_validators_via_get_validators` factor out the common pattern |
| `src/gnosis.rs` | `preserve_system_address_for_aura` (the EIP-158-disable v2 fix); block-reward call returns `(balance_increments, reward_logs)` so InitiateChange detection can read the logs; `rewrite_aura_bytecodes` (per-block bytecode replacements) |
| `src/evm/factory.rs` | `transact_system_call` is reworked: 30M gas, `disable_base_fee`/`disable_block_gas_limit`/`disable_nonce_check` swapped on for the call's duration, SYSTEM_ADDRESS injection with `Created\|Touched`, fee-collector removed, unchanged storage slots filtered out |
| `src/evm/gnosis_evm.rs` | `reward_beneficiary` skips basefee for "free" txs (gasPrice=0 AND priority_fee=0) — service-transaction handling; custom `sstore_eip1283` instruction for Gnosis Constantinople (EIP-1283 net gas metering, which revm only enables at Istanbul via EIP-2200) |
| `src/network.rs` | Reports `final_paris_total_difficulty` instead of `0` when our DB's `head.total_difficulty` is zero — pre-merge AuRa peers refuse a TD=0 advertisement at block N>0 |

---

## 3. The dance, end-to-end

The trickiest interaction is between the rolling-finality tracker and the block executor. The state is shared via `Arc<Mutex<RollingFinality>>` and crosses two distinct phases per block.

```
        ┌──────────────────────────────────────────────────────────┐
        │ src/evm_config.rs : context_for_block(block)             │
        │ ────────────────────────────────────────────────────     │
        │  • compute_finalize_change_address(block)                │
        │      ─ list→contract transition? -> Some(contract)       │
        │      ─ otherwise                                         │
        │      ─ rolling_finality.take_finalize_change(block)      │
        │          ─ pending was finalized -> Some(contract)       │
        │  • validator_contract = aura.validators.contract_at(N)   │
        │  • block_rewards_override = aura reward transitions      │
        │  • aura_bytecode_rewrites = rewrites at this block       │
        └──────────────────────────────────────────────────────────┘
                                  │
                                  ▼  (per-block GnosisBlockExecutionCtx)
        ┌──────────────────────────────────────────────────────────┐
        │ src/block.rs : apply_pre_execution_changes()             │
        │ ────────────────────────────────────────────────────     │
        │  if pre-merge AND posdao AND validators empty:           │
        │      refresh_validators_via_get_validators(...)          │
        │  apply Balancer / AuRa bytecode rewrites                 │
        │  if pre-merge AND finalize_change_address.is_some():     │
        │      aura_system_call_and_commit(finalizeChange)         │
        │      if posdao: refresh_validators_via_get_validators()  │
        │  blockhash + beacon_root standard system calls           │
        └──────────────────────────────────────────────────────────┘
                                  │
                                  ▼   (transactions execute)
        ┌──────────────────────────────────────────────────────────┐
        │ src/block.rs : finish()                                  │
        │ ────────────────────────────────────────────────────     │
        │  apply_post_block_system_calls (gnosis.rs)               │
        │      ─ withdrawals (post-Shanghai)                       │
        │      ─ block reward → returns (balances, reward_logs)    │
        │  if pre-merge AND validator_contract.is_some():          │
        │      scan receipts + reward_logs for InitiateChange      │
        │      if posdao:  rolling_finality.add_pending_transition │
        │      else:       rolling_finality.set_immediate_finalize │
        │  if pre-merge AND posdao:                                │
        │      rolling_finality.push(block_num, signer)            │
        │          ─ may trigger finalization, which schedules     │
        │            a finalizeChange for a future block           │
        └──────────────────────────────────────────────────────────┘
```

A few things to note while reviewing:

- **Two layers of "is_pre_merge"**. The header itself says (`aura_step.is_some()`); the chain spec says (`!is_paris_active_at_block(N)`). Validation uses the header (`mod.rs::is_aura_header`). Execution uses the chain spec (`block.rs`, `evm_config.rs`). They agree at the merge block, but the gating is asymmetric.

- **`SYSTEM_ADDRESS` preservation is unconditional for the block-rewards call**, but pre-merge-only for the validator system calls (init/finalizeChange). The block-rewards call needs preservation across the merge boundary too — see commit `9688f40 fixing merge regressions` and the explicit comment in `gnosis.rs::apply_block_rewards_contract_call`. Empirically validated by the 30M-block sync.

- **`rolling_finality` is `Arc<Mutex<RollingFinality>>`** and crosses three boundaries: (1) `evm_config.rs` builds the per-block ctx and inspects/clears scheduled `finalizeChange`; (2) `block.rs::apply_pre_execution_changes` reads/writes via the validator system calls; (3) `block.rs::finish` writes via `add_pending_transition` / `set_immediate_finalize` / `push`. Locks are held only briefly. The disk-persisted subset (`pending_transitions`, `finalize_change_at`) is the minimum needed to schedule a future `finalizeChange` correctly across restarts.

- **Why `refresh_validators_via_get_validators` instead of using the `InitiateChange` event payload**: the event includes pending validators not yet active (e.g., 16 vs 13). Calling `getValidators()` after `finalizeChange()` returns the *active* set. See "Issue 10" in `aura-pre-merge-implementation.md`.

- **`is_paris_active_at_block` is a block-number check on Gnosis**, which works only because we set `activation_block_number` to the known merge height in `gnosis_spec.rs`. We deliberately keep `fork_block: None` in the same struct because that field flows into the fork-ID computation, and any non-None value would produce a fork-ID that's incompatible with Nethermind/Erigon and break P2P. Both are needed; both are subtle.

---

## 4. External references that appear in the code

These are the canonical implementations that informed each Gnosis-specific behavior. URLs work — paths inside repos may drift, but the file names are stable.

### Nethermind (https://github.com/NethermindEth/nethermind)

| Concern | File |
|---|---|
| `InitiateChange` / `finalizeChange` lifecycle | `src/Nethermind/Nethermind.Consensus.AuRa/Validators/ContractBasedValidator.cs` |
| Rolling finality | `src/Nethermind/Nethermind.Consensus.AuRa/AuRaBlockFinalizationManager.cs` |
| Validator set persistence | `src/Nethermind/Nethermind.Consensus.AuRa/Validators/ValidatorStore.cs` |
| Bytecode rewrites | `src/Nethermind/Nethermind.Consensus.AuRa/Contracts/...` (`ContractRewriter` historically) |
| **EIP-158 disabled for system calls** (the source of `preserve_system_address_for_aura`) | `src/Nethermind/Nethermind.Consensus.AuRa/Transactions/SystemTransactionProcessor.cs` |
| **`!tx.IsFree()` basefee gate** on London+ (matches `gnosis_evm.rs::reward_beneficiary`) | `src/Nethermind/Nethermind.Evm/TransactionProcessing/TransactionProcessor.cs` (`PayFees`) |
| Receipt root with `skipStateAndStatus` fallback | `src/Nethermind/Nethermind.Blockchain/Receipts/ReceiptsRootCalculator.cs` |

### geth (Gnosis fork)

| Concern | File |
|---|---|
| `RollingFinality`, epoch management, `hasSigner` rule | `consensus/aura/aura.go` |
| Validator set extraction from events | `consensus/aura/validators.go` |

### EIPs / specs cited in code

- EIP-158 / EIP-161 — empty-account state clearing (we *selectively disable* this for AuRa system calls)
- EIP-1283 — net SSTORE gas metering (Gnosis activates at Constantinople, mainnet did not)
- EIP-1559 — basefee, with Gnosis variant routing it to `feeCollector`
- EIP-2935, EIP-4788 — blockhash and beacon-root system calls, gated by Prague/Cancun (no-op pre-merge on Gnosis)
- https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md
- https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md

### What we ported verbatim (with small adaptations)

- The seal-hash field set (`compute_seal_hash` in `seal.rs`) matches OpenEthereum / Nethermind's RLP layout — standard 13 fields plus optional EIP extensions, *excluding* `aura_step` and `aura_seal`. A unit golden test against chiado block 100,000 in `aura/seal.rs::tests` locks this in.
- The proposer round-robin (`validators[step % len]`) matches all three reference clients.
- The rolling-finality majority rule (`sign_count.len() * 2 > validators.len()`) matches geth exactly.

---

## 5. Reviewing tips

- **Validate against the 30M-block sync**: the strongest correctness signal is that running the binary against gnosis mainnet from genesis to block 30M produces matching state roots at every 500k checkpoint. If a change touches consensus or system-call paths, it should re-validate at least chiado→1M and gnosis→25.5M (the merge transition).
- **Watch the `is_pre_merge` gate** in `block.rs` and `gnosis.rs::apply_block_rewards_contract_call` — these are the load-bearing branches for "AuRa-only logic vs. always-run."
- **Inspect `Arc<Mutex<RollingFinality>>` sites** if you suspect a livelock or invariant violation. There are exactly four: `validator_count` read, `set_validators` write, `take_finalize_change` write, and `push`/`set_immediate_finalize`/`add_pending_transition` writes. Each holds the lock for one statement.
- **Run `cargo test --lib`** — the unit-test suite covers the algorithmic pieces (seal hash via golden, rolling finality state machine, validator set transitions, `compute_finalize_change_address`, ABI decoder). 56 tests.
- **`docs/aura-pre-merge-implementation.md`** has the full debug history (Issues 1–17) — useful when diagnosing a regression because each issue describes the *symptom* observable in logs.
