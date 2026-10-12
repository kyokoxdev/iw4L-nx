//! SP actor routes over the level's path nodes, on the bots' A\*. Fork-only;
//! kept out of the parent file so upstream merges touch it as little as
//! possible.

use bevy::prelude::Res;
use net::AuthorityWorld;

use super::{EXPAND_CAP, NavEdge, NavGraph, PathError, RouteSearch, TraversalKind, nearest_within};

/// Snap range from an actor or its goal to the nearest path node.
const PATH_NODE_SNAP_IN: f32 = 512.0;

/// SP actors route over the level's path nodes: links are walk edges at their
/// path distance, searched by the same A\* as bot routes.
struct PathNav(NavGraph);

impl sim::script::ActorNav for PathNav {
    fn route(&self, from: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>> {
        let graph = &self.0;
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
}

fn path_nav(nodes: &[sim::script::PathNode]) -> std::sync::Arc<dyn sim::script::ActorNav> {
    let n = nodes.len();
    let adj = nodes
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
    std::sync::Arc::new(PathNav(NavGraph {
        nodes: nodes.iter().map(|node| node.origin).collect(),
        component: vec![0; n],
        adj,
        ..Default::default()
    }))
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
