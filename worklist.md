The docs/specs/snapshot-image-admission.md:1 defines this boundary.

  ## Revised premise

  - Image may contain invalid structure or inaccurate types.
  - Decoding protects the host from malformed bytes.
  - Admission proves resolved structure only.
  - SnapshotImage records successful admission.
  - Restore accepts only SnapshotImage.
  - Trusted capture produces SnapshotImage by construction.
  - Arbitrary editing returns the state to Image.
  - The interpreter converts wrong image values into machine faults.
  - Each VM boundary checks the copied value against verified code.
  - No serialized proof data is required.

  Strange or inaccurate machine values remain valid until use.

  ## Revised issues

  ### 1. Critical: Admission is not an enforced API boundary

  Before Group A, codec::decode performed semantic checks and returned mutable Image.

  Image exposed public fields. Restore accepted Image directly.

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

  Status: implemented.

  ### 2. Critical: Wrong image values can reach trusted assertions

  Image data cannot prove every value type without rejecting valid state.

  A wrong value can reach an accessor after restore.

  An unchecked accessor can panic in the host.

  Resolution:

  - Make each image-reachable accessor fallible.
  - Convert each wrong tag into TypeMismatch.
  - Convert each wrong structural position into MalformedState.
  - Record each boundary type from verified code.
  - Check the value during the boundary copy.
  - Keep nested snapshot bytes opaque until restore.

  This work implements the docs/specs/snapshot-image-admission.md:156.

  The expected boundary type never comes from image data.

  Status: implemented.

  ### 3. High: Future request tokens can become valid later

  A snapshot can contain a token with a future ordinal.

  That token initially fails. It becomes valid when the target later reaches that ordinal.

  This behavior can affect another VM. It is not a type error.

  Several request and mailbox counters also use unchecked addition. Maximum values can panic or wrap.

  Resolution:

  - Reject each token whose ordinal is not below the target counter.
  - Keep stale tokens legal.
  - Keep exact runtime checks for live tokens.
  - Use checked ordinal allocation.
  - Convert exhaustion into a local machine fault.
  - Saturate mailbox metrics.

  Trusted capture always satisfies the ordinal rule. Restore needs no request epoch.

  Status: implemented.

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

  Status: implemented.

  ### 5. High: Proc creation multiplies resource ceilings

  Child creation copies almost the complete VmConfig.

  Each child therefore receives another large fuel and heap ceiling. A child tree multiplies those values.

  Terminal proc records and heaps also remain resident.

  Per-VM limits cannot protect the process from aggregate growth.

  Resolution:

  - Keep VmLimits as local ceilings.
  - Add one `WorldBudget` for the root VM and every proc it spawns.
  - Charge live machines, heap bytes, objects, and resources to that ledger.
  - Add a bounded aggregate execution budget.
  - Make child creation charge the shared ledger.
  - Never copy a consumable aggregate balance.
  - Refund temporary reservations after failure.
  - Release transient charges after termination.
  - Compact terminal machines into bounded terminal records.
  - Retain only generation data and reachable terminal results.
  - Keep the entire record set under the world machine limit.

  This design preserves per-VM isolation without multiplying host resources.

  Status: implemented.

  ### 6. High: Decoding and admission lack aggregate budgets

  LoadLimits applies several limits independently to each machine.

  A compact container can expand into allocations far beyond its byte size.

  A nested snapshot stays opaque until its own restore.

  Container hashing also copies and hashes the same bytes several times.

  Resolution:

  - Add one DecodeBudget for the complete container.
  - Charge all vectors, strings, bytes, and objects.
  - Use checked size arithmetic before every reservation.
  - Use fallible collection reservation.
  - Add one AdmissionBudget.
  - Charge every table entry, stored record, and graph edge.
  - Hash the container without copying its prefix.
  - Compute each required hash once.

  The decoder remains narrow. Admission owns structural graph work.

  Status: implemented.

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

  ### 1. Add regression tests for the known holes

  Add crafted cases for substituted operands, initialization, shared
  generic objects, native values, mailboxes, and terminal results. Keep
  each invalid state representable as Image.

  ### 2. Split decoding from admission

  - Keep wire checks inside decode.
  - Move structural machine and world checks into admission.
  - Return editable Image from the low-level decoder.
  - Return SnapshotImage from load_external.
  - Give SnapshotImage private immutable fields.
  - Restrict unchecked sealing to trusted capture.
  - Remove bare Image from trusted caches.
  - Make inspection of invalid Image total and non-panicking.

  Preserve existing container diagnostics where their stage remains correct.

  ### 3. Harden restored-state access

  - Preserve the uninitialized local marker.
  - Make operand reads fallible.
  - Make object tag reads fallible.
  - Check each restored index before use.
  - Convert a wrong tag into TypeMismatch.
  - Convert malformed state into MalformedState.

  This step contains wrong values inside one machine.

  ### 4. Check values at VM boundaries

  - Record each expected boundary type during bytecode verification.
  - Resolve generic types through the live frame environment.
  - Check each value during its required copy.
  - Fault the sender when the value does not match.
  - Keep a nested SnapshotImage opaque until restore.

  The expected type comes from verified code, not image data.

  ### 5. Fix identity and apply one reviewed format change

  - Hash every semantic OpDef field.
  - Include snapshot classification.
  - Add required initialization and boundary type fields.
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
  - Keep assertions covered by structural admission and preservation.
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

  Test that a failure mid-build leaves the target unchanged.

  Status: implemented.

  ### 8. Reject future request ordinals and check counters

  - Reject a token at or above the target counter.
  - Keep stale token behavior unchanged.
  - Use checked request ordinal allocation.
  - Saturate mailbox metrics.
  - Return local faults on exhaustion.

  Test future tokens, maximum counters, repeated restore, and multi-shot restore.

  Status: implemented without request epochs.

  ### 9. Add aggregate proc-tree accounting

  - Separate local limits from aggregate budgets.
  - Add one `WorldBudget` for the root VM and all spawned procs.
  - Charge child creation to the ledger.
  - Charge all live heap storage.
  - Charge live host resources.
  - Add aggregate scheduler work limits.
  - Compact terminal proc state.
  - Keep all accounting fail-atomic.

  Test deep proc trees under the 4 GiB process cap.

  Status: implemented.

  ### 10. Add aggregate decode and admission accounting

  - Add DecodeBudget.
  - Add AdmissionBudget.
  - Use fallible reservations.
  - Remove repeated container hashing.
  - Add compact-input expansion tests.
  - Add deep and wide graph tests.

  Nested images use lazy admission. Each nested restore starts its own budgets.

  Status: implemented.

  ### 11. Replace scheduler scans

  - Add deterministic ready and blocked indexes.
  - Remove full-machine polling.
  - Add bounded execution quanta.
  - Move blocking host waits outside scheduling.
  - Remove terminal machines from active indexes.
  - Preserve deterministic interleaving tests.
  - Confirm no full-machine scan remains.

  ### 12. Consolidate snapshot capture work

  - Add CapturePlan.
  - Reuse machine and object ordinal maps.
  - Reuse preflight results.
  - Remove linear ordinal searches.
  - Remove repeated graph walks.
  - Stream encoding and hashing where practical.
  - Confirm no linear ordinal search remains.

  ## Delivery groups

  Group A contains worklist items 1 through 6. It establishes the new admission boundary.

  Group B contains items 7 through 10. It establishes failure and resource containment. Group B is complete.

  Group C contains items 11 and 12. It removes known scaling defects.

  Do not start the scheduler rewrite before Group A stabilizes. The admission changes alter several scheduler-visible snapshot tests.
