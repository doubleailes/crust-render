## ADDED Requirements

### Requirement: Thread-held streamed tiles count against the budget

The tiles of streamed UV textures that render threads hold SHALL count against
`CRUST_TEX_CACHE_MB`:
- All threads together SHALL hold at most half the budget. A thread's capacity SHALL
  shrink to fit that limit when there are many threads or the budget is small, down to
  holding nothing.
- A tile held by both the shared cache and a thread SHALL be counted once.
- A tile the shared cache has evicted while a thread still holds it SHALL count until no
  thread holds it. While such tiles push the total over the budget, the shared cache
  SHALL evict to make room.
- As before, the budget SHALL remain a target that concurrent inserts may exceed
  briefly.

`--stats` SHALL report the peak bytes that threads held after the shared cache evicted
them, beside the shared cache's peak resident bytes and budget. The size of the
per-thread caches SHALL NOT change the image.

#### Scenario: A scene that fits the budget

- **WHEN** every tile a render streams fits within `CRUST_TEX_CACHE_MB`
- **THEN** no tile is evicted, and `--stats` reports no bytes held by threads after
  eviction

#### Scenario: A budget smaller than the working set

- **WHEN** a scene is rendered with `CRUST_TEX_CACHE_MB` below the size of the tiles it
  streams
- **THEN** the shared cache's resident bytes plus the bytes threads still hold after
  eviction stay within the budget, apart from what concurrent inserts briefly add, and
  `--stats` reports both

#### Scenario: The budget does not change the image

- **WHEN** the same streamed scene is rendered at `-s 16` with the default
  `CRUST_TEX_CACHE_MB` and with a budget small enough that no thread can hold a tile
- **THEN** the two EXRs are bit-identical, and the smaller budget's render still
  succeeds
