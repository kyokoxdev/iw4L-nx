//! SP actor routes over the level's path nodes, on the bots' A\*. Fork-only;
//! kept out of the parent file so upstream merges touch it as little as
//! possible.

use bevy::prelude::Res;
use net::AuthorityWorld;

use super::{EXPAND_CAP, NavEdge, NavGraph, PathError, RouteSearch, TraversalKind, nearest_within};

/// Snap range from an actor or its goal to the nearest path node.
const PATH_NODE_SNAP_IN: f32 = 512.0;

/// SP actors route over the level's path nodes: links are walk edges at their
/// path distance, searched by the same A\* as bot routes. `traversals` are
/// negotiation (begin, end) node pairs; the actor crosses one with its animscript.
struct PathNav {
    graph: NavGraph,
    traversals: Vec<(usize, usize)>,
}

impl sim::script::ActorNav for PathNav {
    fn route(
        &self,
        from: [f32; 3],
        to: [f32; 3],
        blocked: &[sim::script::Bounds],
    ) -> Option<Vec<[f32; 3]>> {
        let pruned;
        let graph = if blocked.is_empty() {
            &self.graph
        } else {
            // ponytail: copies the graph per blocked route; keep a pruned copy
            // per `disconnectpaths` change if routing shows in profiles.
            let mut copy = self.graph.clone();
            let nodes = &copy.nodes;
            for (from, edges) in copy.adj.iter_mut().enumerate() {
                edges.retain(|edge| {
                    !blocked
                        .iter()
                        .any(|b| crosses(nodes[from], nodes[usize::from(edge.to)], b))
                });
            }
            pruned = copy;
            &pruned
        };
        let start = nearest_within(graph, from, PATH_NODE_SNAP_IN)?;
        let goal = nearest_within(graph, to, PATH_NODE_SNAP_IN)?;
        let mut search = RouteSearch::begin(graph, &[(start, 0.0)], &[goal]);
        // Goals are set rarely and the node graph is small: finish the search now.
        let steps = loop {
            let mut budget = EXPAND_CAP;
            match search.advance(graph, &mut budget) {
                Err(PathError::BudgetExhausted { .. }) => {}
                result => break result.ok()?,
            }
        };
        Some(steps.iter().map(|step| step.position).chain([to]).collect())
    }

    fn traversal_at(&self, at: [f32; 3]) -> Option<(usize, usize)> {
        self.traversals
            .iter()
            .copied()
            .find(|&(start, _)| self.graph.nodes[start] == at)
    }
}

/// Height above the node floor a link is tested at (waist of a standing actor).
const LINK_HEIGHT: f32 = 36.0;

/// Whether the link from `a` to `b` passes through the box (slab test).
fn crosses(a: [f32; 3], b: [f32; 3], (mins, maxs): &sim::script::Bounds) -> bool {
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for k in 0..3 {
        let lift = if k == 2 { LINK_HEIGHT } else { 0.0 };
        let (p, d) = (a[k] + lift, b[k] - a[k]);
        if d.abs() < f32::EPSILON {
            if p < mins[k] || p > maxs[k] {
                return false;
            }
            continue;
        }
        let (mut near, mut far) = ((mins[k] - p) / d, (maxs[k] - p) / d);
        if near > far {
            std::mem::swap(&mut near, &mut far);
        }
        t0 = t0.max(near);
        t1 = t1.min(far);
        if t0 > t1 {
            return false;
        }
    }
    true
}

fn key<'a>(node: &'a sim::script::PathNode, name: &str) -> Option<&'a str> {
    node.keys
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.as_str())
}

/// (begin, end) indices: a `Begin` node whose `target` names an `End` node's `targetname`.
fn traversals(nodes: &[sim::script::PathNode]) -> Vec<(usize, usize)> {
    nodes
        .iter()
        .enumerate()
        .filter(|(_, begin)| begin.kind == "Begin")
        .filter_map(|(b, begin)| {
            let target = key(begin, "target")?;
            let end = nodes
                .iter()
                .position(|end| end.kind == "End" && key(end, "targetname") == Some(target))?;
            Some((b, end))
        })
        .collect()
}

fn path_nav(nodes: &[sim::script::PathNode]) -> std::sync::Arc<dyn sim::script::ActorNav> {
    let n = nodes.len();
    let mut adj: Vec<Vec<NavEdge>> = nodes
        .iter()
        .map(|node| {
            node.links
                .iter()
                .filter(|(to, _)| usize::from(*to) < n)
                .map(|&(to, cost)| NavEdge {
                    to,
                    cost,
                    kind: TraversalKind::Walk,
                })
                .collect()
        })
        .collect();
    let traversals = traversals(nodes);
    for &(begin, end) in &traversals {
        if adj[begin].iter().any(|edge| usize::from(edge.to) == end) {
            continue;
        }
        let cost = nodes[begin]
            .origin
            .iter()
            .zip(nodes[end].origin)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();
        adj[begin].push(NavEdge {
            to: end as u16,
            cost,
            kind: TraversalKind::Walk,
        });
    }
    std::sync::Arc::new(PathNav {
        graph: NavGraph {
            nodes: nodes.iter().map(|node| node.origin).collect(),
            component: vec![0; n],
            adj,
            ..Default::default()
        },
        traversals,
    })
}

/// SP actors route over the level's path nodes with the bots' A\*.
pub(crate) fn install_actor_nav(world: Option<Res<AuthorityWorld>>) {
    let Some(mut world) = world.as_ref().and_then(|world| world.try_lock()) else {
        return;
    };
    if let Some(nodes) = world.path_nodes_awaiting_nav() {
        let edges: usize = nodes.iter().map(|node| node.links.len()).sum();
        diag::info!(
            Sim,
            "actors: route graph over {} path nodes, {edges} links",
            nodes.len()
        );
        world.install_actor_nav(path_nav(&nodes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::script::{ActorNav, PathNode};

    /// A begin node with no walk link still routes to its end node.
    #[test]
    fn traversal_joins_begin_to_end() {
        let node = |kind, key_name, value: &str, origin| PathNode {
            kind,
            origin,
            keys: vec![(key_name, value.to_owned())],
            ..Default::default()
        };
        let nodes = [
            node("Begin", "target", "end_a", [0.0, 0.0, 0.0]),
            node("End", "targetname", "end_a", [100.0, 0.0, 64.0]),
        ];
        assert_eq!(traversals(&nodes), vec![(0, 1)]);
        let nav = path_nav(&nodes);
        assert_eq!(nav.traversal_at([0.0, 0.0, 0.0]), Some((0, 1)));
        assert!(
            nav.route([0.0, 0.0, 0.0], [100.0, 0.0, 64.0], &[])
                .is_some()
        );
        let door = ([40.0, -10.0, 0.0], [60.0, 10.0, 128.0]);
        assert!(crosses([0.0; 3], [100.0, 0.0, 0.0], &door));
        assert!(!crosses([0.0, 50.0, 0.0], [100.0, 50.0, 0.0], &door));
    }
}
