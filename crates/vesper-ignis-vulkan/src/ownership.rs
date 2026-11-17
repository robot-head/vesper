//! Queue-family ownership transfer planning (no `unsafe`).
//!
//! With `VK_SHARING_MODE_EXCLUSIVE` resources, a resource used by a queue
//! family other than the one that owns it needs a *release* barrier on the
//! owning family's queue and a matching *acquire* barrier on the new one,
//! ordered by a semaphore (§19.9; Vulkan spec "Queue Family Ownership
//! Transfer"). Ignis plans cross-queue handoffs with timeline fences already,
//! so this module only decides where the two halves go: given the segments
//! of one submission in submission order, each tagged with its queue family
//! and the resources it touches, it returns one [`Transfer`] per ownership
//! change. The release goes at the end of the last segment that used the
//! resource on the old family (before that segment's signals), and the
//! acquire at the start of the first segment on the new family (after its
//! waits). A use whose contents are discarded (a texture whose first barrier
//! starts from `UNDEFINED`) takes ownership without a transfer, as the spec
//! allows.

use std::collections::BTreeMap;

/// One resource use by a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Use<R> {
    pub resource: R,
    /// The segment discards the previous contents (no transfer is needed).
    pub discards: bool,
}

/// An ownership change of `resource` from `from_family` to `to_family`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer<R> {
    pub resource: R,
    /// The segment that releases ownership; `None` when the resource was
    /// last used by an earlier submission on `from_family`.
    pub release_in: Option<usize>,
    pub acquire_in: usize,
    pub from_family: u32,
    pub to_family: u32,
}

/// Plans the transfers of one submission. `segments[i]` is
/// `(family, uses)` in submission order; `owners` holds each resource's
/// owning family before the submission and is updated to the owners after
/// it.
pub fn plan<R: Copy + Ord>(
    segments: &[(u32, Vec<Use<R>>)],
    owners: &mut BTreeMap<R, u32>,
) -> Vec<Transfer<R>> {
    let mut last_use: BTreeMap<R, usize> = BTreeMap::new();
    let mut out = Vec::new();
    for (i, (family, uses)) in segments.iter().enumerate() {
        for u in uses {
            match owners.get(&u.resource).copied() {
                Some(owner) if owner != *family && !u.discards => {
                    // Several uses of one resource in one segment transfer
                    // once: the owner is updated below.
                    out.push(Transfer {
                        resource: u.resource,
                        release_in: last_use.get(&u.resource).copied(),
                        acquire_in: i,
                        from_family: owner,
                        to_family: *family,
                    });
                }
                _ => {}
            }
            owners.insert(u.resource, *family);
            last_use.insert(u.resource, i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(r: u8) -> Use<u8> {
        Use {
            resource: r,
            discards: false,
        }
    }

    #[test]
    fn handoffs_between_families_release_and_acquire_once() {
        // Family 0 (graphics) writes 1, family 1 (compute) reads it and
        // writes 2, family 0 reads both back.
        let segments = vec![
            (0, vec![u(1)]),
            (1, vec![u(1), u(1), u(2)]),
            (0, vec![u(1), u(2)]),
        ];
        let mut owners = BTreeMap::new();
        let t = plan(&segments, &mut owners);
        assert_eq!(
            t,
            vec![
                Transfer {
                    resource: 1,
                    release_in: Some(0),
                    acquire_in: 1,
                    from_family: 0,
                    to_family: 1
                },
                Transfer {
                    resource: 1,
                    release_in: Some(1),
                    acquire_in: 2,
                    from_family: 1,
                    to_family: 0
                },
                Transfer {
                    resource: 2,
                    release_in: Some(1),
                    acquire_in: 2,
                    from_family: 1,
                    to_family: 0
                },
            ]
        );
        assert_eq!(owners, BTreeMap::from([(1, 0), (2, 0)]));
    }

    #[test]
    fn discarded_contents_and_one_family_need_no_transfer() {
        let mut owners = BTreeMap::from([(7u8, 1u32)]);
        let segments = vec![
            (
                0,
                vec![Use {
                    resource: 7,
                    discards: true,
                }],
            ),
            (0, vec![u(7), u(8)]),
        ];
        assert!(plan(&segments, &mut owners).is_empty());
        // Ownership carried from an earlier submission is released there.
        let mut owners = BTreeMap::from([(7u8, 1u32)]);
        let t = plan(&[(0, vec![u(7)])], &mut owners);
        assert_eq!(t[0].release_in, None);
        assert_eq!((t[0].from_family, t[0].to_family), (1, 0));
    }
}
