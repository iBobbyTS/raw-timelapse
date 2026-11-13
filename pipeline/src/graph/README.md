# In-run graph

`--graph FILE` reads the same JSON model as `default.json`. A node has a unique
`id`, a registered `kind`, named `inputs` pointing to producer IDs, and optional
`waits` containing `{ "node": "id", "scope": "frame" | "sequence" }`.
Unknown fields, built-ins, ports, types, undeclared IDs and combined data/event
cycles are errors before native preparation. The inventory's exact frame IDs and
EOF count are checked before dispatch. Runtime plan identity/order/gains must
match that inventory.

Registered kinds and inputs:

| Kind | Inputs | Output / execution |
|---|---|---|
| source | none | ordered source IDs, stream |
| prepare | source | prepared references, stream |
| references | frames | unchanged references, stream |
| barrier | frames | ordered references after exact sealed EOF, sequence |
| solve | frames | frozen exposure table after incremental measurement and sealed EOF |
| zero | frames | zero exposure table after sealed EOF; no measurements |
| develop | frames, plan | owned signed FP32 AP1/D60 image, stream |
| identity | image (linear) | unchanged owned linear image, stream |
| exposure | image (linear), config `ev` | explicit additional linear exposure, stream |
| color | image (linear) | existing profile's temporary RGB24 encoder input, stream |
| encode | image (color) | streaming sink with begin/push/end |
| delete | frames, explicit waits required | deletes generated prepared DNG only, stream |

One source, prepare, exposure-table producer, develop, color and encoder are
required. Reference streams can fork and cross repeated sequence barriers;
references and small measurements may grow with sequence length. Image outputs
have one consumer and one slot. The scheduler drains downstream first and starts
another develop only after all image slots are empty, keeping one full-resolution
image in the final chain. A sequence wait on an image ancestor is rejected because
it would prevent that bounded chain from draining. Image batching and image
fanout are outside this model; sequence ports carry lightweight references or
the exposure table.

Each stream node emits frame completion after its operation succeeds and closes
its input read; sequence completion follows EOF/drain. Sequence-only nodes emit
sequence completion. Dependencies require both data and the declared event.
Events are emitted once in the invocation, with node ID and frame ID when scoped
to a frame; they are separate from pixel data. This is in-memory scheduling, with
no cross-run replay or persistent ownership system.

The default delete node waits for develop's frame completion. The native process
has closed/reaped and its FP32 payload is owned in memory at that point. Delete
runs before image consumers, independently of encoder reads. Removing delete
retains generated cache paths under the partial directory after publication;
reconnecting its wait changes cleanup timing. Direct-source DNGs are never deleted.
Explicit reference readers must be connected before deletion when they need DNG
access. There is no general future-reader or malicious-configuration analysis.

Strength zero replaces any solve node with zero in the selected graph. It uses
the same scheduler and skips all analysis. The enabled solver retains its internal
quality/coverage record for the run and checks its accounting before admitting the
read-only plan. This record is absent from product reports; no gain probes,
full-frame feedback passes or encoded temporal audit run in the product graph.

SIGINT/SIGTERM set a Rust stop flag. Scheduling and completion emission check it;
owned process monitors interrupt blocked native reads. Cancellation uses SIGTERM
and a three-second Rust escalation deadline. Native handles cancellation by
terminating its own independently spawned Converter process group, allowing
500 ms for cooperative exit, then SIGKILL and wait/reap of its child. Native exits
before Rust finishes its wait. Encoder failure cancels the registered native
producer; protocol/operation errors drop/reap owned native and encoder processes.
No interrupted frame is completed or final video published. Cache/partial paths
and process exits remain in the existing logs. Host SIGKILL/crash recovery and
arbitrary descendant recovery are outside the contract.

The existing H.264 geometry/color adapter and report files remain temporary
endpoints. Encoder configuration and product CLI/log migration have separate
owners; the graph never invokes the old whole-source encoder orchestration.
