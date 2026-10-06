## ADDED Requirements

### Requirement: Learned occlusion proxies are opt-in

The kernel SHALL build a learned occlusion proxy only when it was compiled with the
`neural-bvh` feature and the commit asked for one. A build without the feature SHALL
answer every query and report every footprint exactly as before this change. A proxy
SHALL never change a closest-hit query: `intersect` on a proxied scene SHALL return
bit-identical hits to the same scene committed without a proxy.

#### Scenario: Feature off

- **WHEN** the kernel is built without `neural-bvh` and the checked-in samples are
  rendered at 16 spp
- **THEN** every output EXR is bit-identical to the goldens recorded before this change

#### Scenario: Closest hit ignores the proxy

- **WHEN** the same eligible mesh is committed with and without a proxy, and the same
  rays are cast at both with `intersect`
- **THEN** every hit is bit-identical: distance, barycentrics, normal and ids

### Requirement: Only eligible scenes get a proxy

A proxy SHALL be built only for a committed scene whose geometry is triangle meshes,
directly or through static instances, and has no motion. The scene SHALL also hold at
least the requested minimum number of triangles, counting instanced ones. For any other
scene the request SHALL be ignored, the scene SHALL commit exactly as without it, and
the kernel SHALL report that no proxy was built.

#### Scenario: A scene with curves

- **WHEN** a scene holding a triangle mesh and a curve is committed with a proxy
  request
- **THEN** no proxy is built and every query is answered exactly

#### Scenario: A small scene

- **WHEN** a mesh below the requested triangle threshold is committed with a proxy
  request
- **THEN** no proxy is built

#### Scenario: A prototype of instanced parts

- **WHEN** a scene whose only geometry is static instances of triangle meshes, holding
  more triangles than the threshold, is committed with a proxy request
- **THEN** a proxy is built

### Requirement: Proxied occlusion is approximate and falls back to exact

On a proxied scene, an `occluded` query whose ray mask matches the mask the proxy was
trained for SHALL be answered by the proxy. The answer is an approximation and MAY
differ from the exact answer. Any query with another mask SHALL be answered exactly.
The probe in the next requirement measures the approximation error, and no requirement
bounds it.

#### Scenario: Another mask is exact

- **WHEN** a proxy trained for shadow rays is queried with `occluded` using a camera
  ray mask
- **THEN** the answer is the exact kernel's answer for every ray

#### Scenario: A segment that misses the scene bounds

- **WHEN** a proxied scene is queried with a segment that misses its bounds
- **THEN** the answer is "not occluded", exactly as the exact kernel answers

### Requirement: Proxy training is deterministic

Training a proxy SHALL depend only on the committed geometry and the request. The same
input SHALL produce bit-identical proxy parameters and bit-identical query answers on
every run and at every thread count.

#### Scenario: Training twice at different thread counts

- **WHEN** the same eligible mesh is committed with a proxy request once with one
  worker thread and once with eight
- **THEN** both proxies answer the same set of rays with bit-identical results

### Requirement: Proxy memory is reported

The kernel's memory footprint SHALL report the bytes held by occlusion proxies as a
line of its own, counted once per shared instanced scene. The line SHALL be zero when
no proxy was built.

#### Scenario: A proxied prototype placed many times

- **WHEN** one proxied scene is placed through 1 000 instances
- **THEN** the proxy line counts that scene's proxy once

### Requirement: The proxy is measured against the exact kernel

The kernel's `ray_throughput` example SHALL accept `--neural`. With it, the example
SHALL build proxies for its fixture scenes, in cache and with `--large` out of cache.
For each scene it SHALL print the throughput of proxied and exact occlusion over the
same rays, the false-positive and false-negative rates, the training time and the proxy
bytes.

#### Scenario: Running the probe

- **WHEN** `ray_throughput --neural` is run on a build with the `neural-bvh` feature
- **THEN** each fixture scene prints exact and proxied Mray/s, the false-positive and
  false-negative percentages, the training seconds and the proxy bytes
