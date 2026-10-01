## MODIFIED Requirements

### Requirement: Ptex

Ptex textures SHALL be addressed by the mesh's authored face index (the base cage for
subdivided meshes).

A `.ptx` that would preload in less than `CRUST_PTEX_STREAM_MIN_MB` SHALL be preloaded,
mip-reduced under `CRUST_PTEX_MAX_LOG2`. A larger one SHALL stream through the reader's
cache by default. `CRUST_PTEX_STREAM=0` SHALL preload every texture.

A streamed texture's mip chain SHALL follow `CRUST_PTEX_STREAM_MIPSPACE`:

- **`capped`** (default): levels finer than the preload cap are read from the file;
  the cap level is the file's level at the cap resolution; coarser levels are reduced
  in linear light from the cap level, by the same reduction the preloaded pyramid
  uses.
- **`file`**: every level is read from the file.
- **`linear`**: a mipmapped `.ptx` SHALL NOT stream, and is preloaded.

Under `capped`, every texel a lookup reads at a resolution the preloaded texture holds
SHALL be bit-identical to the preloaded texel.

#### Scenario: Streaming is the default for a large file

- **WHEN** a render binds a mipmapped `.ptx` larger than `CRUST_PTEX_STREAM_MIN_MB`
  with no Ptex environment variables set
- **THEN** the texture streams, and `--stats` reports it as streamed under the
  `capped` chain

#### Scenario: Small files still preload

- **WHEN** a render binds only `.ptx` files smaller than `CRUST_PTEX_STREAM_MIN_MB`
- **THEN** every texture is preloaded, and the image is bit-identical to a render with
  `CRUST_PTEX_STREAM=0`

#### Scenario: Capped streaming matches the preload where both hold the texels

- **WHEN** a mipmapped `.ptx` whose faces exceed the cap is evaluated streamed under
  `capped` and preloaded, at footprints no finer than one cap texel
- **THEN** every evaluated value is bit-identical

#### Scenario: Capped streaming resolves authored detail

- **WHEN** a lookup's footprint is finer than one cap texel on a face authored above
  the cap
- **THEN** the streamed texture reads a stored file level finer than the cap, where the
  preloaded texture reads its cap level

#### Scenario: The linear policy still refuses

- **WHEN** `CRUST_PTEX_STREAM_MIPSPACE=linear` is set and the `.ptx` carries mip
  levels
- **THEN** the texture is preloaded and `--stats` names the reason
