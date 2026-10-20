## MODIFIED Requirements

### Requirement: Refused products

A product whose resolved camera or resolution differs from the render's
SHALL be skipped with one warning naming it. A `deepRaster` product SHALL be
written as an OpenEXRId matte (see the `aovs` requirement "Identity and ID
mattes") when it holds a var naming what its ids stand for (`primId`,
`materialId` or `assetId`), and skipped with one warning naming it
otherwise.

#### Scenario: Deep product

- **WHEN** a product authors `productType = "deepRaster"` and only a
  `depth` var
- **THEN** a warning is logged and no file is written for it, while the other
  products are written

#### Scenario: Deep product as an ID matte

- **WHEN** a product authors `productType = "deepRaster"` and a `primId` var
- **THEN** a deep scanline EXR with channels `A`, `Id` and `Z` is written at
  its `productName`
