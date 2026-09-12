# Next steps toward product quality

Work proposal, 2026-09-12, with the scope decision below accepted by the owner.
The implementation steps and acceptance tests are **not results**.
The [current proof](current.md) and [accepted design](../docs/superpowers/specs/2026-09-11-clix-design.md)
remain authoritative. Preserve the tested source manifest before the next code
change. Work remains in the existing checkout on master, without cloning.
The owner later authorized private pushes for publication preparation; public
visibility remains a separate decision.

The working release scenario is concrete: work on server, use a granted tool on
laptop, inspect the work from either machine, and continue after the laptop
returns. Complete and prove one milestone before relying on it for the next.
These names are public pseudonyms for the tested machines. This deployment
scenario does not limit the product to laptops or to a two-machine topology.

## Authority scope — decided 2026-09-12

The current owner socket trusts the Unix UID. An unrestricted agent using that
UID has owner authority on that machine. A CLI role label or another prompt
cannot supply the missing distinction.

The owner chose: **"just limit on laptop"**. The agent may use the normal
development account on server. Clix limits its remote access to laptop through
grants made by the laptop owner. The server agent may execute granted tools and
request others; it cannot approve requests or alter laptop grants over Clix.
Local agent isolation on server is outside this release scope. No separate server
account, agent sandbox or alternative grant policy is part of this work.

The laptop owner account remains trusted. This is not a claim that Clix can
restrict an agent already given unrestricted owner access on the laptop through
some other route. A tool grant also permits that binary's capabilities; current
Clix does not sandbox the binary or constrain its arguments. Review effects on
laptop owner state and credentials when assessing the actual safety of a hand.

The existing owner socket and grants remain in place. This decision resolves
the deployment scope; it is not new implementation or proof. Laptop authority
tests remain mandatory: remote owner operations and attempted grant widening
must fail, including through any newly added history or recovery protocol.

## 1. Complete shared job history

Separate execution admission from replicated history first. The current
`job::dispatch_pending` selects deliveries from fields in `Store.jobs`.
Inserting imported history into that collection without changing admission
could create or suppress executable work. Keep one job identity and explicit
local authority to deliver or run it. Learning about work cannot authorize it.

Define who owns each fact before implementing synchronization:

- Origin: invocation, destination, queueing and observations about delivery.
- Runner: acceptance, execution outcome, captured output and uncertainty after
  an interrupted run. A caller timeout does not prove execution stopped.
- Authenticate publications and bind them to the invocation. Duplicate or stale
  facts cannot regress completed work or restart it. Use authority and valid
  transitions to resolve state; wall-clock timestamps are not authority.

Synchronize incrementally with resumable, idempotent delivery. Include local
Clix jobs and failures before delivery, as well as accepted remote runs. Expose
stale or incomplete history when another body is unavailable.

Acceptance: real process tests cover local/remote work, denial, pin failure,
lost acknowledgements and restart of either side. Duplicate, reordered or
forged history causes **zero executions and zero grant changes**. Repeat the
relevant cases on laptop/server and verify eventual convergence of IDs, invocation,
authoritative outcome and output. Instant agreement is not promised.

## 2. Make recovery an ordinary owner operation

Provide inspection by job ID, retrieval of saved output, and an explanation of
waiting, denied, failed and uncertain outcomes. Let the owner select a pending
request by ID; terminal and native notification actions use the same objects.

Inspection must preserve uncertain outcomes and once reservations. An explicit
new attempt has a new ID linked to the old attempt. It cannot rewrite the old
outcome or prove that the old command had no effects. Unknown outcomes do not
trigger automatic re-execution.

For pin recovery, let the owner inspect live/retained versions and resolve the
receipt through version-checked operations. Retain recoverable data until the
decision is durable. Normal recovery must not require editing JSON or guessing
which hidden filename contains an owner's edit.

Acceptance: interrupt execution, file publication and recovery itself. Verify
documented recovery through Clix, preservation after an interrupted resolution,
and no implicit grant widening or rerun. Remote clients cannot invoke owner
recovery decisions.

## 3. Establish operating limits

Measure latency, memory use, storage growth and sync cost with a representative
source tree and growing history. The current whole-state JSON writes and
retained file versions have not been proved at that scale. Choose any storage
replacement from those requirements and measurements.

Add bounded retention and crash-safe migration/upgrade. Compaction must preserve
receipts needed to prevent duplicate execution. Disk exhaustion must stop
admission before unrecorded work runs. Test failures using isolated storage,
without filling either working machine's home filesystem.

Agree on pin synchronization triggers and supported file types, then implement
and prove them. The current pairing/before-execution behavior is not continuous
background replication. Make stale/conflicting state visible.

Keep grant evaluation and job transitions centralized. Remove duplicate state
or abstractions when an observable invariant justifies it. Independent review
should focus on authority, lost data and retries.

## 4. Prove daily use on the actual machines

With the owner present, test physical laptop suspend/resume while work runs on
server. Observe that suspend actually occurred, server work continued, and the
hand waited and resumed under its current grant. Include expiry/revocation
during the outage. A closed lid or refused connection alone is not sleep proof.

Exercise Allow once, Allow and Deny on the real desktop, pending replay after
restart, and tray state. Repeat the owner workflow through the CLI with no
display and no agent. Test installation, upgrade, restart and recovery on both
actual distributions. The original two-Arch criterion remains separately
unproved unless the owner explicitly changes that milestone.

Then use the actual server/laptop workflow for a proposed week. Record every manual
intervention, stalled request and recovery failure with its job ID and installed
artifact. Fix those failures and repeat their scenarios before broadening use.

The release gate is evidence that ordinary use and expected failures require no
developer-only repair while the documented authority and location claims stay
true. The automated suite is necessary evidence, not the entire gate.
