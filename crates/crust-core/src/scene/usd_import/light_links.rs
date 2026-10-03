//! `collection:lightLink` and `collection:shadowLink`: which receivers a
//! light illuminates, and which occluders shadow it.
//!
//! Membership is openusd's own (`Collection::compute_membership_query`), so
//! every `UsdCollectionAPI` rule — nearest opinion wins, `includeRoot`,
//! expansion rules, nested collections with cycles broken — is the
//! reference's. What crust adds is *when* it is asked. A light can be
//! traversed after the geometry it links, and when streaming in a different
//! chunk whose stage is gone by the end, so nothing is decided at emission:
//!
//! - As the traversal dispatches each prim that emits geometry, it records the
//!   first `geom_id` that prim assigned and its stage path, interned
//!   ([`LightLinks::saw`]). Ids are handed out in traversal order, so one entry
//!   covers a mesh, a native instance, a whole `PointInstancer` and an area
//!   light's own emitter. Volume regions are recorded the same way.
//! - Each light with a restricting link has its queries computed when it is
//!   met, while its chunk's stage is live ([`LightLinks::light_added`]).
//! - After the last chunk, [`LightLinks::resolve`] evaluates each distinct
//!   path against every linked light, deduplicates the answers into
//!   *classes*, and installs them: a per-`geom_id` light-class table in the
//!   world, a per-light illuminated-class set, and the shadow-class encoding
//!   in the geometry masks (design D3 of `add-light-and-shadow-linking`).
//!
//! Receivers are judged by the *stage* path of the prim that brings them in —
//! the instance or instancer prim, never a prototype's `/__Prototype_N` path —
//! so a target inside a prototype cannot tell instances apart (warned).
//!
//! A light whose `lightLink` includes no receiver at all illuminates nothing
//! and leaves the light list: a camera-visible light at infinity becomes a
//! backdrop (the Moana island's `sky_dome_cam_llc`), an area light keeps only
//! its camera-visible geometry.

use std::collections::HashMap;

use openusd::sdf;
use openusd::usd::{Collection, ExpansionRule, MembershipQuery, PathRule, Prim, Stage};
use tracing::{debug, warn};

use crate::light::{EVERY_CLASS, Light, LightLinks as RuntimeLinks, LightList};
use crate::ray::{MASK_CAMERA, MASK_SHADOW, RayMask};
use crate::rt_world::WorldBuilder;
use crate::volume::VolumeRegion;

use super::attrs::{custom_bool, custom_token, value_at};
use super::prim_at;

/// The mask bits that carry shadow classes (design D3): 3–30 for the most
/// populated classes, 31 shared by the rest.
const CLASS_BITS: u32 = !0b111;
/// The shared overflow bit.
const OVERFLOW_BIT: u32 = 1 << 31;
/// How many shadow classes get a bit of their own (3–30).
const ALLOCATED: usize = 28;

/// Reads one of a light's link collections (`lightLink` / `shadowLink`) as a
/// membership query, or `None` when it restricts nothing (UsdLux's fallback:
/// `includeRoot = 1` and no `includes` / `excludes`).
///
/// UsdLux gives these two collections an `includeRoot` fallback of **true**,
/// where `UsdCollectionAPI`'s is false — and openusd applies the latter. So
/// when `includeRoot` is not authored the pseudo-root is added to the rule map
/// here, unless an opinion on `/` is already there.
pub(super) fn link_query(stage: &Stage, prim: &Prim, name: &str) -> Option<MembershipQuery> {
    let prop = |suffix: &str| format!("collection:{name}:{suffix}");
    let targets = |suffix: &str| {
        prim.relationship(prop(suffix))
            .targets()
            .unwrap_or_default()
    };
    let (includes, excludes) = (targets("includes"), targets("excludes"));
    let include_root = custom_bool(prim, &prop("includeRoot"));
    let expression = value_at(&prim.attribute(prop("membershipExpression"))).is_some();
    if expression {
        warn!(
            "{}: collection:{name} authors membershipExpression, which crust does not \
             read — the collection is read as the default (every prim)",
            prim.path()
        );
        return None;
    }
    if includes.is_empty() && excludes.is_empty() && include_root != Some(false) {
        return None;
    }
    let collection = match Collection::new(prim.path().clone(), name) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "{}: collection:{name} unreadable ({e}) — read as the default",
                prim.path()
            );
            return None;
        }
    };
    let query = match collection.compute_membership_query(stage) {
        Ok(q) => q,
        Err(e) => {
            warn!(
                "{}: collection:{name} unreadable ({e}) — read as the default",
                prim.path()
            );
            return None;
        }
    };
    for t in includes.iter().chain(&excludes) {
        if inside_instance(stage, t) {
            warn!(
                "{}: collection:{name} targets {t}, inside an instance — membership is \
                 judged on the instance, so the target cannot be told apart from its \
                 siblings",
                prim.path()
            );
            break;
        }
    }
    if include_root.is_some() {
        return Some(query);
    }
    let rule = match collection.expansion_rule(stage).unwrap_or_default() {
        ExpansionRule::ExplicitOnly => return Some(query),
        ExpansionRule::ExpandPrims => PathRule::ExpandPrims,
        ExpansionRule::ExpandPrimsAndProperties => PathRule::ExpandPrimsAndProperties,
    };
    let mut map = query.rule_map().clone();
    map.entry(sdf::Path::abs_root()).or_insert(rule);
    Some(MembershipQuery::new(map))
}

/// A link collection's authored opinions, as far as membership depends on
/// them — what two stages must agree on to be interchangeable for it.
#[derive(PartialEq)]
struct Opinions {
    includes: Vec<sdf::Path>,
    excludes: Vec<sdf::Path>,
    include_root: Option<bool>,
    expansion_rule: Option<String>,
    expression: bool,
}

fn opinions(prim: &Prim, name: &str) -> Opinions {
    let prop = |suffix: &str| format!("collection:{name}:{suffix}");
    let targets = |suffix: &str| {
        prim.relationship(prop(suffix))
            .targets()
            .unwrap_or_default()
    };
    Opinions {
        includes: targets("includes"),
        excludes: targets("excludes"),
        include_root: custom_bool(prim, &prop("includeRoot")),
        expansion_rule: custom_token(prim, &prop("expansionRule")),
        expression: value_at(&prim.attribute(prop("membershipExpression"))).is_some(),
    }
}

/// Whether some strict ancestor of `path` is a native instance, i.e. `path`
/// names geometry inside a prototype.
fn inside_instance(stage: &Stage, path: &sdf::Path) -> bool {
    let mut p = path.parent();
    while let Some(a) = p {
        if a.is_abs_root() {
            return false;
        }
        if prim_at(stage, a.clone()).is_instance().unwrap_or(false) {
            return true;
        }
        p = a.parent();
    }
    false
}

/// A light with a restricting link, waiting for the last receiver.
struct Pending {
    path: sdf::Path,
    /// Its place in the import's [`LightList`] when it was added.
    index: usize,
    light: Option<MembershipQuery>,
    shadow: Option<MembershipQuery>,
}

/// The traversal's record of receivers and linked lights.
#[derive(Default)]
pub(super) struct LightLinks {
    /// Every prim that emitted geometry or a volume, interned.
    paths: HashMap<sdf::Path, u32>,
    /// `(first geom_id, path)` per emitting prim, in traversal order: the
    /// prim owns every id up to the next entry's.
    runs: Vec<(u32, u32)>,
    /// `(volume index, path)` per volume-region prim.
    volumes: Vec<(u32, u32)>,
    pending: Vec<Pending>,
}

impl LightLinks {
    fn intern(&mut self, path: &sdf::Path) -> u32 {
        if let Some(&id) = self.paths.get(path) {
            return id;
        }
        let id = self.paths.len() as u32;
        self.paths.insert(path.clone(), id);
        id
    }

    /// Records that the prim at `path` emitted the geometries `geoms` (ids
    /// `[start, end)`) and the volume regions `vols`. Nothing when it
    /// emitted neither.
    pub(super) fn saw(
        &mut self,
        path: &sdf::Path,
        geoms: std::ops::Range<usize>,
        vols: std::ops::Range<usize>,
    ) {
        if geoms.is_empty() && vols.is_empty() {
            return;
        }
        let id = self.intern(path);
        if !geoms.is_empty() {
            self.runs.push((geoms.start as u32, id));
        }
        for v in vols {
            self.volumes.push((v as u32, id));
        }
    }

    /// Records a light just added at `index` of the import's light list, if
    /// either of its links restricts it.
    ///
    /// Two stages can answer, and each can be wrong. The traversed `chunk`
    /// has every opinion, payloads included, but a streamed chunk is
    /// composed under a population mask that keeps only its own subtree, so
    /// a collection nested in another subtree (a rig's `Scope` of light
    /// groups, say) resolves to nothing there. `whole`, the index stage, has
    /// every subtree but no payload, so a link a payload authors is missing
    /// there. So `whole` is used only when it holds the light's link
    /// opinions exactly as the chunk does; otherwise the chunk is, and a
    /// nested collection it cannot see is warned about.
    pub(super) fn light_added(
        &mut self,
        whole: Option<&Stage>,
        chunk: &Stage,
        path: &sdf::Path,
        index: usize,
    ) {
        let on_chunk = prim_at(chunk, path.clone());
        let same = |w: &Stage| {
            let on_whole = prim_at(w, path.clone());
            let agrees = on_whole.is_valid().unwrap_or(false)
                && ["lightLink", "shadowLink"]
                    .iter()
                    .all(|n| opinions(&on_whole, n) == opinions(&on_chunk, n));
            agrees.then_some(on_whole)
        };
        let (stage, prim) = match whole.and_then(|w| same(w).map(|p| (w, p))) {
            Some(found) => found,
            None => {
                for name in ["lightLink", "shadowLink"] {
                    let missing = opinions(&on_chunk, name).includes.into_iter().find(|t| {
                        openusd::usd::is_collection_api_path(t).is_some_and(|(owner, _)| {
                            !prim_at(chunk, owner).is_valid().unwrap_or(false)
                        })
                    });
                    if let Some(t) = missing {
                        warn!(
                            "{path}: collection:{name} is authored in a payload and includes \
                             {t}, which this chunk of the streamed import does not compose — \
                             that nested collection contributes nothing"
                        );
                    }
                }
                (chunk, on_chunk)
            }
        };
        let light = link_query(stage, &prim, "lightLink");
        let shadow = link_query(stage, &prim, "shadowLink");
        if light.is_none() && shadow.is_none() {
            return;
        }
        self.pending.push(Pending {
            path: path.clone(),
            index,
            light,
            shadow,
        });
    }

    /// Installs the links once the last receiver has been traversed. Must
    /// run before the light list's selection and the world's commit.
    pub(super) fn resolve(
        self,
        lights: &mut LightList,
        world: &mut WorldBuilder,
        volumes: &mut [VolumeRegion],
    ) {
        if self.pending.is_empty() {
            return;
        }
        // The interned paths, by id: `intern` hands out `0..n` in order, so
        // sorting by id puts each path at its own index.
        let mut by_id: Vec<(&sdf::Path, u32)> = self.paths.iter().map(|(p, &id)| (p, id)).collect();
        by_id.sort_unstable_by_key(|&(_, id)| id);
        let paths: Vec<&sdf::Path> = by_id.into_iter().map(|(p, _)| p).collect();
        // How many geometries (and volumes) each path accounts for: the
        // population the shadow bits are allocated by.
        let n_geoms = world.count();
        let mut weight = vec![0usize; paths.len()];
        for (k, &(start, id)) in self.runs.iter().enumerate() {
            let end = self.runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
            weight[id as usize] += (end - start) as usize;
        }
        for &(_, id) in &self.volumes {
            weight[id as usize] += 1;
        }

        // 1. Lights whose `lightLink` includes no receiver leave the list,
        //    from the highest index down so each removal leaves the indices
        //    still to remove in place.
        let mut nothing: Vec<(usize, &sdf::Path)> = Vec::new();
        let mut kept: Vec<&Pending> = Vec::new();
        for p in &self.pending {
            match &p.light {
                Some(q) if !paths.iter().any(|path| q.is_path_included(path)) => {
                    nothing.push((p.index, &p.path))
                }
                _ => kept.push(p),
            }
        }
        nothing.sort_unstable_by_key(|&(index, _)| std::cmp::Reverse(index));
        for &(index, path) in &nothing {
            demote(lights, world, index, path);
        }
        // Indices of the lights that stay, after the removals above.
        let shifted = |index: usize| index - nothing.iter().filter(|&&(r, _)| r < index).count();

        let n = lights.count();
        let mut links = RuntimeLinks {
            illuminates: vec![None; n],
            shadow_masks: vec![MASK_SHADOW; n],
            nee_only: vec![false; n],
        };
        let mut any = false;

        // 2. Light-link classes: receivers with the same answer for every
        //    linked light share one.
        let linked: Vec<Linked> = kept
            .iter()
            .filter_map(|p| Linked::new(shifted(p.index), p, p.light.as_ref()))
            .collect();
        if !linked.is_empty() {
            let (class_of, classes) = classify(&paths, linked.iter().map(|l| l.query));
            if classes.len() >= EVERY_CLASS as usize {
                warn!(
                    "{} light-link classes exceed crust's {}; light links are ignored",
                    classes.len(),
                    EVERY_CLASS
                );
            } else {
                let mut table = vec![EVERY_CLASS; n_geoms];
                for (k, &(start, id)) in self.runs.iter().enumerate() {
                    let end = self.runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
                    let class = class_of[id as usize] as u16;
                    for slot in &mut table[start as usize..end as usize] {
                        *slot = class;
                    }
                }
                world.set_light_classes(table);
                for &(v, id) in &self.volumes {
                    volumes[v as usize].light_class = class_of[id as usize] as u16;
                }
                for (bit, l) in linked.iter().enumerate() {
                    let mut set = vec![0u64; classes.len().div_ceil(64)];
                    let mut members = 0;
                    for (c, key) in classes.iter().enumerate() {
                        if key.get(bit) {
                            set[c / 64] |= 1 << (c % 64);
                            members += 1;
                        }
                    }
                    debug!(
                        "{}: collection:lightLink illuminates {members} of {} receiver \
                         class(es)",
                        l.path,
                        classes.len()
                    );
                    links.illuminates[l.index] = Some(set.into_boxed_slice());
                }
                any = true;
            }
        }

        // 3. Shadow classes, encoded in the masks' free bits (design D3).
        let restricted: Vec<Linked> = kept
            .iter()
            .filter_map(|p| Linked::new(shifted(p.index), p, p.shadow.as_ref()))
            .collect();
        if !restricted.is_empty() {
            let receivers = Receivers {
                paths: &paths,
                weight: &weight,
                runs: &self.runs,
                vols: &self.volumes,
            };
            receivers.encode_shadows(&restricted, world, volumes, &mut links);
            any = true;
        }

        if any {
            lights.set_links(links);
        }
    }
}

/// A light that stays in the list, with one of its two link queries: where
/// it sits once the lights that illuminate nothing are gone, and the link it
/// restricts.
struct Linked<'a> {
    index: usize,
    path: &'a sdf::Path,
    query: &'a MembershipQuery,
}

impl<'a> Linked<'a> {
    /// `None` when `query` is — the light does not restrict that link.
    fn new(index: usize, light: &'a Pending, query: Option<&'a MembershipQuery>) -> Option<Self> {
        Some(Linked {
            index,
            path: &light.path,
            query: query?,
        })
    }
}

/// A bit vector over the linked lights: which of them a receiver is a
/// member of.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key(Vec<u64>);

impl Key {
    fn get(&self, bit: usize) -> bool {
        self.0[bit / 64] & (1 << (bit % 64)) != 0
    }
}

/// Each path's class and the classes' keys, over the given queries.
fn classify<'q>(
    paths: &[&sdf::Path],
    queries: impl Iterator<Item = &'q MembershipQuery>,
) -> (Vec<u32>, Vec<Key>) {
    let queries: Vec<&MembershipQuery> = queries.collect();
    let words = queries.len().div_ceil(64);
    let mut ids: HashMap<Key, u32> = HashMap::new();
    let mut classes: Vec<Key> = Vec::new();
    let class_of = paths
        .iter()
        .map(|path| {
            let mut key = Key(vec![0u64; words]);
            for (bit, q) in queries.iter().enumerate() {
                if q.is_path_included(path) {
                    key.0[bit / 64] |= 1 << (bit % 64);
                }
            }
            *ids.entry(key.clone()).or_insert_with(|| {
                classes.push(key);
                classes.len() as u32 - 1
            })
        })
        .collect();
    (class_of, classes)
}

/// Every receiver the traversal recorded, as shadow linking reads them.
struct Receivers<'a> {
    /// The interned paths, by id.
    paths: &'a [&'a sdf::Path],
    /// How many geometries (and volumes) each path accounts for: the
    /// population the shadow bits are allocated by.
    weight: &'a [usize],
    /// [`LightLinks::runs`] and [`LightLinks::volumes`].
    runs: &'a [(u32, u32)],
    vols: &'a [(u32, u32)],
}

impl Receivers<'_> {
    /// Design D3. Class 0 — occluders every restricted light is shadowed by —
    /// keeps `MASK_SHADOW`; every other shadow caster clears it and carries one
    /// class bit (3–30 by population, 31 shared). Unrestricted lights' rays carry
    /// `MASK_SHADOW` and every class bit, so they are blocked exactly as before.
    fn encode_shadows(
        &self,
        restricted: &[Linked],
        world: &mut WorldBuilder,
        volumes: &mut [VolumeRegion],
        links: &mut RuntimeLinks,
    ) {
        let (class_of, classes) = classify(self.paths, restricted.iter().map(|l| l.query));
        let full = |key: &Key| (0..restricted.len()).all(|b| key.get(b));
        // Population per class, then the bit each gets.
        let mut population = vec![0usize; classes.len()];
        for (path, &c) in class_of.iter().enumerate() {
            population[c as usize] += self.weight[path];
        }
        let mut order: Vec<usize> = (0..classes.len()).filter(|&c| !full(&classes[c])).collect();
        order.sort_by_key(|&c| std::cmp::Reverse(population[c]));
        // `None` for class 0 (keeps MASK_SHADOW), else the class's bit.
        let mut bit_of: Vec<Option<u32>> = vec![None; classes.len()];
        for (rank, &c) in order.iter().enumerate() {
            bit_of[c] = Some(if rank < ALLOCATED {
                1 << (3 + rank)
            } else {
                OVERFLOW_BIT
            });
        }
        let encode = |mask: RayMask, class: usize, authored: &mut bool| -> RayMask {
            let high = mask.0 & CLASS_BITS;
            if high != 0 && high != CLASS_BITS {
                *authored = true;
            }
            let base = mask.0 & !CLASS_BITS;
            if base & MASK_SHADOW.0 == 0 {
                // Not a shadow caster: no class bit, so no shadow ray matches it.
                return RayMask(base);
            }
            match bit_of[class] {
                None => RayMask(base),
                Some(bit) => RayMask((base & !MASK_SHADOW.0) | bit),
            }
        };
        let mut authored = false;
        let n_geoms = world.count();
        for (k, &(start, id)) in self.runs.iter().enumerate() {
            let end = self.runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
            let class = class_of[id as usize] as usize;
            for g in start..end {
                let m = encode(world.mask(g), class, &mut authored);
                world.set_mask(g, m);
            }
        }
        for &(v, id) in self.vols {
            let region = &mut volumes[v as usize];
            region.mask = encode(region.mask, class_of[id as usize] as usize, &mut authored);
        }
        if authored {
            warn!(
                "crust:rayMask bits 3-31 are rewritten by shadow linking, which encodes \
             occluder classes in them"
            );
        }
        let overflow: Vec<usize> = order.iter().skip(ALLOCATED).copied().collect();
        // Unrestricted lights: blocked by every caster, whatever its class.
        for m in links.shadow_masks.iter_mut() {
            *m = RayMask(MASK_SHADOW.0 | CLASS_BITS);
        }
        for (bit, l) in restricted.iter().enumerate() {
            if classes.iter().all(|k| k.get(bit)) {
                // Shadowed by everything after all: an ordinary light, MIS kept.
                debug!("{}: collection:shadowLink includes every occluder", l.path);
                continue;
            }
            let in_overflow = overflow.iter().filter(|&&c| classes[c].get(bit)).count();
            if in_overflow != 0 && in_overflow != overflow.len() {
                warn!(
                    "{}: collection:shadowLink cannot be encoded ({} occluder classes share \
                 crust's overflow bit and it includes only some) — shadowed by every \
                 occluder",
                    l.path,
                    overflow.len()
                );
                continue;
            }
            let mut mask = MASK_SHADOW.0;
            for (c, b) in bit_of.iter().enumerate() {
                if let Some(b) = b
                    && *b != OVERFLOW_BIT
                    && classes[c].get(bit)
                {
                    mask |= b;
                }
            }
            if in_overflow != 0 {
                mask |= OVERFLOW_BIT;
            }
            let excluded = order.iter().filter(|&&c| !classes[c].get(bit)).count();
            debug!(
                "{}: collection:shadowLink ignores {excluded} of {} occluder class(es) — sampled \
             by NEE alone at non-delta vertices",
                l.path,
                classes.len()
            );
            links.shadow_masks[l.index] = RayMask(mask);
            links.nee_only[l.index] = true;
        }
    }
}

/// Takes the light at `index` out of the list: it illuminates nothing. A
/// light at infinity the camera sees becomes a backdrop; an area light keeps
/// its geometry for camera rays only, if the camera saw it.
fn demote(lights: &mut LightList, world: &mut WorldBuilder, index: usize, path: &sdf::Path) {
    let (light, escape_mask) = lights.remove(index);
    let role = if light.at_infinity() {
        if escape_mask.sees(MASK_CAMERA) {
            lights.add_backdrop(light);
            "a camera-only backdrop"
        } else {
            "dropped (hidden from the camera too)"
        }
    } else if let Some(id) = light.geom_id() {
        let mask = world.mask(id) & MASK_CAMERA;
        world.set_mask(id, mask);
        if mask.sees(MASK_CAMERA) {
            "camera-visible geometry only"
        } else {
            "dropped (hidden from the camera too)"
        }
    } else {
        "dropped"
    };
    debug!("{path}: collection:lightLink includes no receiver — illuminates nothing, {role}");
}
