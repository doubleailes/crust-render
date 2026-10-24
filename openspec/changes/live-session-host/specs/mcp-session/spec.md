## ADDED Requirements

### Requirement: A host session follows an owned layer stack

A live session a host drives SHALL import a root layer and follow the layers
the host names as its own: the root and sublayers the root reaches through
`subLayers` with no offset. After the host rewrites any of them, the session
SHALL diff each owned layer whose bytes changed and classify the union of the
changes as an edit batch of the override layer is classified, and SHALL apply
it in place or by a re-import accordingly. A render after a change applied in
place SHALL be bit-identical to a fresh session's render of the same files.

#### Scenario: An edit in an owned sublayer applies in place

- **WHEN** a host owns `graph.usda`, a sublayer of its root, and changes a
  light's `inputs:intensity` there
- **THEN** the change is applied as `lights`, and the render is bitwise the
  render of a fresh session on the same files

#### Scenario: A layer the host does not own

- **WHEN** a sublayer of the root that the host did not name changes on disk
- **THEN** the next change is applied as `full`

#### Scenario: An owned layer outside the root layer stack

- **WHEN** a host names a layer the root does not reach through `subLayers`
  with no offset
- **THEN** loading is refused, naming the layer

### Requirement: A host can stop the render without waiting

A host SHALL be able to stop the running render from any thread without
waiting for an import or update in progress. The render SHALL stop once the
per-pixel sample advances in flight finish, and report that it was cancelled.

#### Scenario: Cancel during a long render

- **WHEN** a host cancels a 4096 spp render after its first image
- **THEN** the render returns cancelled, and the session does not report it
  converged
