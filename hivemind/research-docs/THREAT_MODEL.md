# Threat model (capabilities + provenance)

Security claims in this project are limited to what is stated and tested here.

## Assets
Agent memory (blobs in the memory graph), the model-tool interface (COM3), the mesh
(COM2), provenance records, workflow outputs.

## Principals
| Principal | Trusted? | Checked by capabilities? |
|---|---|---|
| kernel, shell (human admin), desktop, disk restore | trusted | no |
| model acting for the shell (`ai` command) | trusted | no |
| agent `<id>` | **untrusted** | yes |
| model acting for agent `<id>` | untrusted, **checked as that agent** | yes |
| peer VM (anything arriving on the mesh) | **untrusted** | yes, as subject `peer` |

## Adversaries in scope
1. **Compromised or buggy agent** trying to read or write nodes outside its grant, or
   to invoke the model tool.
2. **Confused deputy via the model**: a model answer written on behalf of an agent
   that lacks write access (the model is checked with the agent's rights).
3. **Hostile peer VM** writing or planting nodes over the mesh.
4. **Self-escalation**: an agent obtaining capabilities it was not granted.

## Mechanism
* Enforcement point: `Hive::write_blob_as` (every write path), the agent read that
  makes a rule fire, `authorize_tool("ai")` before an offload, and node creation by peers.
* Default-deny once `cap enforce on`; off by default for backward compatibility.
* Capabilities: subject, resource (`node:<id>`, `ns:<prefix>`, `tool:<name>`), ops
  (`r w i d`), optional expiry (ticks), issuer, revocation. Only the shell issues them.
  Delegation never exceeds the parent's ops, resource or expiry; revocation cascades.
* Every decision is a provenance event with the matched capability or the denial
  reason and the policy version; `cap explain <event>` / `policy explain` report it.
* Persistence: only the enforcement flag is saved (fail-closed). Capabilities are
  runtime-only because agent ids are re-issued at every boot; a saved grant to
  "agent 2" could otherwise pass to a different agent after reboot.

## Tested (see RESULTS.md)
Unauthorized read and write denied; cross-agent access denied; tool invocation without
capability denied; expiry; revocation including delegated children; delegation cannot
widen ops, namespace or lifetime; a non-holder cannot delegate; a peer without a grant
cannot write or create nodes; an authorized write cites the allow event that permitted it.

Measured end to end on a real guest with the real 3B model (`experiments/mvp_injection.py`):
a peer's write reached the model through a three-agent chain, the `taint` query flagged
the tool call and did not flag the trusted control, and the model's answer was refused
because the reporting agent held read-only access.

## Known gap shown by that experiment
An `ai:` offload sends the **whole node** as context. A reporting agent whose own rule
depends only on `summary` still forwards the peer-written `doc` to the model. Taint
detection makes this visible after the fact; it does not block it. Blocking would need
either per-key context selection or a policy that refuses tool calls whose ancestors
include untrusted writes. Neither is implemented.

## Out of scope (not claimed)
* Agents in this OS cannot execute commands; "agent self-grant" is prevented
  structurally (no path to the capability table) rather than by a policy check.
* A compromised kernel, shell, host, or hypervisor.
* Side channels (timing, paging), hardware attacks, denial of service.
* Mesh authenticity: the wire has no sender identity, sequence numbers or MAC, so
  replay and spoofing between peers are possible; capabilities only bound *what* a
  peer may write.
* Prompt injection detection. The system records whether untrusted content reached a
  protected action; it does not decide whether content is malicious.
* Formal verification; the policy language is the code in `cap_core.rs`.
