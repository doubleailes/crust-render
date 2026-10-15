//! Render passes: one USD `RenderPass` prim rendered in one invocation, its
//! image split into render layers.
//!
//! "Pass" alone means a sampling sweep inside the engine (a guided render's
//! passes); this module always says **render pass** for the USD prim.
//!
//! A pass names the `RenderSettings` it renders with (`renderSource`), and
//! declares its layers as multiple-apply collections on itself,
//! `collection:layer:<name>`, in `apiSchemas` order. Whatever no layer claims
//! belongs to the implicit [`DEFAULT_LAYER`], always last. Its outputs are
//! grouped into files by a file pattern, which the host
//! expands: the engine only carries the authored strings.

/// The implicit layer of a pass: every prim no layer collection claims, and
/// every camera ray that escapes. Always the pass's last layer.
pub const DEFAULT_LAYER: &str = "default";

/// The file pattern of a pass that authors no `crust:filePattern`.
pub const DEFAULT_FILE_PATTERN: &str = "$l_$n_$o.$04f.$x";

/// The layer pattern of a pass that authors no `crust:layerPattern`:
/// `$n_$o`, naming each output inside a shared file by layer and RenderVar.
pub const DEFAULT_LAYER_PATTERN: &str = "$n_$o";

/// What a render pass asks for, as the import read it.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderPass {
    /// The `RenderPass` prim's path.
    pub path: String,
    /// The prim's name: what `$l` and `$L` expand to.
    pub name: String,
    /// The `RenderSettings` prim the pass renders with: its `renderSource`'s
    /// target, else the stage's own settings prim.
    pub render_source: Option<String>,
    /// The layer names, in `apiSchemas` order, [`DEFAULT_LAYER`] last.
    pub layers: Vec<String>,
    /// `crust:filePattern`, else [`DEFAULT_FILE_PATTERN`].
    pub file_pattern: String,
    /// `crust:layerPattern`, else [`DEFAULT_LAYER_PATTERN`].
    pub layer_pattern: String,
}
