The docs/specs/snapshot-image-admission.md:1 defines this boundary.

  ## Revised premise

  - Image may contain invalid structure or inaccurate types.
  - Decoding protects the host from malformed bytes.
  - Admission proves resolved structure and accurate live types.
  - SnapshotImage records successful admission.
  - Restore accepts only SnapshotImage.
  - Trusted capture produces SnapshotImage by construction.
  - Arbitrary editing returns the state to Image.
  - The interpreter can retain trusted unreachable! assertions.
  - No serialized proof data is required.
  - No repeated dynamic type checking is required.

  Strange but well-typed machine state remains valid.

  ## Revised issues

  ### 1. Critical: Admission is not an enforced API boundary

  codec::decode performs semantic checks and returns mutable Image.

  Image exposes public fields. crates/lm-vm/src/snapshot/restore.rs:36 accepts &Image.

  Host code can construct or mutate an image after validation. It can then bypass the external loader.

  The trusted cache also stores Arc<Image>. The type system records no admission fact.

  Resolution:

  - Keep Image editable and untrusted.
  - Make decoding perform container checks only.
  - Add admit(Image, LoadedModule) -> SnapshotImage.
  - Make SnapshotImage immutable.
  - Make restore accept only admitted state.
  - Store SnapshotImage in trusted caches.
  - Restrict trusted promotion to capture code.
  - Remove externally_checked from trust decisions.

  This work implements the docs/specs/snapshot-image-admission.md:40.

  ### 2. Critical: Current type admission is incomplete

  The current checker loses verifier-created substituted types. It represents them as None and skips their values.

  Unit and Uninit currently satisfy every declared type.

  The graph walk records only an object ordinal. It does not record the resolved type used for that visit.

  Generic fields and closure captures use unsubstituted layout types.

  Native values validate only their outer object tags. Their generic protocol parameters remain unchecked.

  Terminal Unit values can bypass their declared result type.

  These holes can reach trusted interpreter assertions such as crates/lm-vm/src/machine.rs:1231.

  Resolution:

  - Add a complete resolved-type representation.
  - Preserve every verifier-created substitution.
  - Derive operand types at each saved program point.
  - Track initialization separately from Unit.
  - Validate graph pairs as (machine, object, resolved type).
  - Apply substitutions to fields and captures.
  - Validate every terminal result type.
  - Validate every mailbox message type.
  - Validate pending arguments from pre-perform verifier state.
  - Validate native types through their target relationships.
  - Validate nested snapshots under the same admission budget.

  This work implements the docs/specs/snapshot-image-admission.md:156.

  The interpreter can retain its trusted assertions after this work.

  ### 3. High: Request tokens can become valid later

  A snapshot can contain a token with a future ordinal.

  That token initially fails. It becomes valid when the target later reaches that ordinal.

  This behavior can affect another VM. It is not a type error.

  Several request and mailbox counters also use unchecked addition. Maximum values can panic or wrap.

  Resolution:

  - Add a fresh request epoch to each restored machine.
  - Relocate only tokens matching a current pending request.
  - Give relocated current tokens the fresh epoch.
  - Leave every other restored token permanently stale.
  - Mint future tokens with the fresh epoch.
  - Use checked ordinal allocation.
  - Convert exhaustion into a local machine fault.
  - Use checked mailbox counter updates.

  Admission does not prove token provenance. Runtime identity rules prevent token resurrection.

  ### 4. High: Restore lacks one atomic preparation stage

  Restore modifies the target before all work succeeds.

  Successful restore can retain existing policy-table entries. This violates fresh default-deny authority.

  Failure can replace the target machine. That replacement can lose its prior configuration.

  Guest restore installs the world before it allocates the returned handle. A later allocation failure exposes installed state.

  Restore also clamps configuration without checking all live state.

  Source fuel, mailbox limits, frames, stacks, and heap state can exceed effective target limits.

  Resolution:

  - Add a detached RestorePlan.
  - Reserve every machine and resource before construction.
  - Calculate every effective limit before allocation.
  - Clamp remaining fuel and future limits.
  - Reject existing state above an effective limit.
  - Build fresh default-deny policy tables.
  - Build every heap and machine outside the live world.
  - Prepare the caller reply before commit.
  - Make commit perform no fallible allocation.
  - Return every reservation when preparation fails.
  - Leave the target unchanged after every failure.

  This work implements the docs/specs/snapshot-image-admission.md:335.

  ### 5. High: Proc creation multiplies resource ceilings

  Child creation copies almost the complete VmConfig.

  Each child therefore receives another large fuel and heap ceiling. A child tree multiplies those values.

  Terminal proc records and heaps also remain resident.

  Per-VM limits cannot protect the process from aggregate growth.

  Resolution:

  - Keep VmLimits as local ceilings.
  - Add one shared WorldBudget ledger.
  - Charge live machines, heap bytes, objects, and resources globally.
  - Add a bounded aggregate execution budget.
  - Make child creation charge the shared ledger.
  - Never copy a consumable aggregate balance.
  - Refund temporary reservations after failure.
  - Release transient charges after termination.
  - Compact terminal machines into bounded terminal records.
  - Retain only generation data and reachable terminal results.
  - Keep the entire record set under the world machine limit.

  This design preserves per-VM isolation without multiplying host resources.

  ### 6. High: Decoding and admission lack aggregate budgets

  LoadLimits applies several limits independently to each machine.

  A compact container can expand into allocations far beyond its byte size.

  Nested snapshots can multiply decoding and verification work.

  The type reader recomputes verifier dataflow for every saved frame.

  Container hashing also copies and hashes the same bytes several times.

  Resolution:

  - Add one DecodeBudget for the complete container.
  - Charge all vectors, strings, bytes, objects, and nested containers.
  - Use checked size arithmetic before every reservation.
  - Use fallible collection reservation.
  - Add one AdmissionBudget.
  - Charge every resolved type and graph pair.
  - Share both budgets with nested snapshot admission.
  - Memoize admitted nested containers by hash and admission identity.
  - Compute verifier states once per function.
  - Reuse those states for every saved frame.
  - Hash the container without copying its prefix.
  - Compute each required hash once.

  The decoder remains narrow. Admission owns graph and type work.

  ### 7. Medium: Scheduler work grows quadratically

  The scheduler scans all machines for blocked and runnable procs.

  It allocates temporary vectors during those scans.

  Completed machine records remain in the scan set.

  One proc can run until termination, blocking, or fuel exhaustion.

  A host wait inside the scheduler can stop all other procs.

  Resolution:

  - Add a deterministic ready index.
  - Add blocked indexes for each wake source.
  - Remove terminal records from runnable indexes.
  - Run each proc for one bounded quantum.
  - Requeue a runnable proc after its quantum.
  - Keep host waiting outside the scheduler.
  - Wake only machines affected by one completion.
  - Preserve deterministic ordering with explicit queue rules.
  - Reset scheduler statistics at each requested boundary.

  Use BTreeSet when lowest identifier order remains observable. Use VecDeque when enqueue order defines determinism.

  ### 8. High: Operation identity omits snapshot classification

  OpDef.snapshot affects snapshot and resource behavior.

  crates/lm-abi/src/lib.rs:466 does not hash that field.

  Changing only this classification can preserve the operation identity. It can also preserve the manifest digest and VerifiedKey.

  Resolution:

  - Add snapshot classification to operation identity.
  - Make the manifest digest cover that field.
  - Bump the relevant ABI version.
  - Bump the snapshot format when wire fields also change.
  - Regenerate pinned identities.
  - Test a classification-only mutation.
  - Confirm the mutation changes VerificationHash.
  - Reject snapshots with the old admission identity.

  The sidecar requires every semantic operation field inside admission identity.

  ### 9. Medium: Snapshot capture repeats graph work

  Capture walks machine references several times.

  It also repeats object traversal during ordering, preflight, and encoding.

  Several lookups use linear Vec::contains and position.

  A wide machine world can therefore approach quadratic work.

  Resolution:

  - Build one reusable CapturePlan.
  - Record the deterministic machine order once.
  - Store machine-to-ordinal indexes.
  - Record each machine object order once.
  - Store object-to-ordinal indexes.
  - Reuse preflight facts during encoding.
  - Reserve the final container once.
  - Stream section hashing where possible.
  - Keep capture work bounded by one aggregate budget.

  ## Items removed from the issue list

  The revised design removes these earlier assumptions:

  - Image does not need permanent validity.
  - Snapshot bytes do not need serialized type proofs.
  - Arbitrary edits do not need incremental proof maintenance.
  - The interpreter does not need routine dynamic type checks.
  - Admission does not need to reject strange typed state.
  - Admission does not prove token history or resource availability.

  Those concerns now belong to editing, runtime identity, or restore planning.

  ## Ordered worklist

  ### 1. Add regression tests and an invariant inventory

  - Add crafted cases for every current admission hole.
  - Keep invalid states representable as Image.
  - Confirm admission rejects each inaccurate type.
  - Confirm restore cannot accept Image.
  - Confirm external loading returns only SnapshotImage.
  - Confirm trusted capture performs no second type scan.
  - Confirm repeated restore performs no admission scan.
  - Inventory every interpreter unreachable! and verified expect.
  - Map each assertion to admission or runtime preservation.
  - Convert any unmapped assertion into a local fault.

  Cover substituted operands, initialization, shared generic objects, native values, mailboxes, and terminal results.

  ### 2. Split decoding from admission

  - Keep wire checks inside decode.
  - Move check_machine, check_world, and type checks into admission.
  - Return editable Image from the low-level decoder.
  - Return SnapshotImage from load_external.
  - Give SnapshotImage private immutable fields.
  - Restrict unchecked sealing to trusted capture.
  - Remove bare Image from trusted caches.
  - Make inspection of invalid Image total and non-panicking.

  Preserve existing container diagnostics where their stage remains correct.

  ### 3. Add complete resolved-type admission

  - Add a resolved-type arena.
  - Cache verifier state for each saved program point.
  - Preserve substituted operand and local types.
  - Track initialization independently.
  - Validate every typed root.
  - Traverse graph pairs by object and resolved type.
  - Apply generic substitutions before field checks.
  - Charge all work to AdmissionBudget.

  This step closes the ordinary interpreter type holes.

  ### 4. Add native relational admission

  - Derive Vm[T] from the target machine.
  - Derive Handle[M,R] from the target proc.
  - Derive PendingCall[A,R] from the pending operation.
  - Check Snapshot[T] against the nested root result.
  - Admit nested SnapshotImage values once.
  - Store admitted nested dependencies with SnapshotImage.
  - Reject missing relational type evidence.

  Choose explicit stored identities only when target derivation cannot supply them.

  ### 5. Fix identity and apply one reviewed format change

  - Hash every semantic OpDef field.
  - Include snapshot classification.
  - Add required initialization or native type fields.
  - Bump affected versions once.
  - Regenerate pins and checked fixtures.
  - Test old container rejection.
  - Test classification-only identity movement.

  Keep unrelated representation changes outside this format change.

  ### 6. Enforce the trusted interpreter boundary

  - Make restore consume admitted state only.
  - Make trusted capture construct SnapshotImage privately.
  - Store admission identity beside canonical bytes.
  - Remove origin-based trust.
  - Audit every trusted interpreter assertion again.
  - Keep assertions covered by admission and preservation.
  - Change uncovered paths into local faults.

  This completes the safety milestone.

  ### 7. Implement detached restore planning

  - Add RestorePlan.
  - Calculate effective limits first.
  - Reserve target resources first.
  - Build detached machines and heaps.
  - Create fresh policy tables.
  - Prepare relocation tables.
  - Prepare the guest reply.
  - Commit without fallible work.
  - Restore the exact prior target after failure.

  Add tests for every allocation and reservation failure point.

  ### 8. Add request epochs and checked counters

  - Add request epochs to machines and tokens.
  - Assign fresh epochs during restore.
  - Relocate only currently valid tokens.
  - Make every other restored token stale.
  - Use checked request ordinal allocation.
  - Use checked mailbox counter updates.
  - Return local faults on exhaustion.

  Test future tokens, maximum counters, repeated restore, and multi-shot restore.

  ### 9. Add aggregate world accounting

  - Separate local limits from aggregate budgets.
  - Add WorldBudget.
  - Charge child creation to the ledger.
  - Charge all live heap storage.
  - Charge live host resources.
  - Add aggregate scheduler work limits.
  - Compact terminal proc state.
  - Keep all accounting fail-atomic.

  Test deep proc trees under the 4 GiB process cap.

  ### 10. Add aggregate decode and admission accounting

  - Add DecodeBudget.
  - Add AdmissionBudget.
  - Use fallible reservations.
  - Share budgets with nested snapshots.
  - Cache verifier dataflow results.
  - Remove repeated container hashing.
  - Add compact-input expansion tests.
  - Add deep and wide graph tests.

  Measure peak memory and total admission work.

  ### 11. Replace scheduler scans

  - Add deterministic ready and blocked indexes.
  - Remove full-machine polling.
  - Add bounded execution quanta.
  - Move blocking host waits outside scheduling.
  - Remove terminal machines from active indexes.
  - Preserve deterministic interleaving tests.

  Measure one-shot completion across increasing proc counts.

  ### 12. Consolidate snapshot capture work

  - Add CapturePlan.
  - Reuse machine and object ordinal maps.
  - Reuse preflight results.
  - Remove linear ordinal searches.
  - Remove repeated graph walks.
  - Stream encoding and hashing where practical.

  Measure deep, wide, and multi-machine snapshots.

  ## Delivery groups

  Group A contains worklist items 1 through 6. It establishes the new admission boundary.

  Group B contains items 7 through 10. It establishes failure and resource containment.

  Group C contains items 11 and 12. It removes known scaling defects.

  Do not start the scheduler rewrite before Group A stabilizes. The admission changes alter several scheduler-visible snapshot tests.
