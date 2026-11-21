# A How to read the Lean models

Before applying a Lean theorem to a Vesper program, read its statement and assumptions. Check which operations the model admits and how it defines the behavior you want to rely on.

## A.1 Match a theorem to the claim you need

The correspondence matrix maps topics such as mode ordering, resumption safety, disjoint projections, and grade arithmetic to declarations. For a statement about a projected call, ask which paths count as disjoint and what appears in the conclusion after return. A proof that counts resources and a proof that preserves each resource’s identity establish different properties.

The code-generation models relate selected abstract instructions to source operations. A validation certificate supplies facts such as operand ranges at each control-flow edge. Kernel replay checks a proof artifact, and an axiom audit inspects its dependencies. Establishing that the deployed compiler produced the modeled rows and machine bytes needs additional evidence.

## A.2 Read the small resource calculus

A small program can mint a fresh handle, pass it through a binding, and consume it. The core calculus can track the identity through those steps and show exhaustion for its stated closed programs. A place capability graph separately records overlapping access paths. A branch join must agree on which linear bindings remain before execution continues.

## A.3 Connect modes, destinations, and refinements

The four-axis model combines field requirements and checks restrictions on ordered guards and capsules. Destination coverage accounts for which pieces an output requires. A refinement snapshot relates an old field value to its updated value. When exchanging linear payloads, the displaced owner must still appear exactly once. Two names for the same place cannot justify a disjoint swap.

Additional models address clock-indexed observations, quotient congruence, handler state, resource bounds, closures, polymorphism, and mutable places. A bridge relates specific operations in two models. For example, the bridge for exchanging typed fields assumes both fields already contain values. Extending it to uninitialized storage would need a separate proof.

## A.4 Compare execution before and after ghost erasure

Start with a computation that acquires and consumes a proof-only token alongside runtime work. Remove the ghost steps and compare the observable runtime result and resource trace. The operational erasure results require the ghost work to terminate and discharge its obligations. The bridge admits closed instrumentation in the runtime calculus. Captured ghost resources require a further argument.

# B The Vesper mark and its visual meaning

The Vesper mark shows an intact record above three separate permissions. The open channels between the blades preserve that meaning in monochrome.

## B.1 Read the field-tree prism

The upper chevron represents an aggregate such as a broker. The gap beneath it represents splitting a set of field projections for a call. The left blade denotes consumption or linear ownership, the center blade exclusive access, and the right blade shared access or an effect capability. Their common outline recalls the caller’s record after the loans end.

## B.2 Construct the silhouette and its cuts

The master uses a 256-by-256 coordinate grid. From the top-left outer corner (30, 34) to the apex (128, 230), a 98-unit horizontal run accompanies a 196-unit fall. That two-to-one slope fixes the outer wall. A matching ray defines the other wall. Place the blade vertices on those rays so the silhouette has no step.

The canonical polygon construction sets the channel width, center blade width, chevron width, and split gap. The vertical channels are 12 units wide in the master. An optical cut is a version of the mark sized for a particular use. At small sizes it adjusts the proportions so the gaps remain visible.

## B.3 Use color and typography consistently

Dusk and Dawn use different foreground and background pairings. The palette uses OKLCH to control lightness, chroma, and hue. Check contrast against the surface where the mark or text actually appears. The wordmark uses Space Grotesk Bold, prose uses Inter, and code uses JetBrains Mono. Scale the mark uniformly to keep the channels and slopes intact.

## B.4 Choose the artifact for the surface

Use the master vector mark when there is room for its geometry. At 16 pixels the favicon uses its own pixel-snapped cut. The horizontal lockup adds the lowercase wordmark. Use the pixel-snapped artwork for a file icon. In a terminal, the character banner fits the text grid.

## B.5 Interpret the geometric proof

In this construction, collinearity means that the chosen vertices lie on one straight ray. Disjointness means the designated polygons have a separating channel. Lean can check those facts about coordinates. Check the rasterized mark at its intended size and measure contrast against its actual background.

## B.6 Reproduce the artwork from parameters

A deterministic emitter takes the approved dimensions and palette and writes the asset. Two runs with identical inputs should reproduce the same geometry. If the favicon needs adjustment, change its optical cut while preserving the master used by documentation and larger surfaces.
