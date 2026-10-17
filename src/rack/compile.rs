//! Rack runtime: compile a project's rack state into the processing plan
//! the renderer follows. The plan is data (indices, ids, edges); the DSP
//! (render.rs, fx.rs) consumes it and never decides graph order itself.

use super::model::{DeviceKind, RackState, Route, StripKind};
use crate::project::Project;
use serde::{Deserialize, Serialize};

/// Bus processing order: a bus is processed after every bus that feeds
/// it. Cycles (rejected by route_bus) fall back to declaration order.
/// (Moved unchanged from render.rs, which now calls it from here.)
pub fn bus_order(p: &Project) -> Vec<usize> {
    topo(p).0
}

/// Buses on (or downstream of) a routing cycle, in declaration order.
/// `route_bus` rejects cycles, so in a project built by the tools this is
/// empty; a hand-edited file can still carry one, and the renderer then
/// processes those buses in declaration order after the others.
pub fn bus_cycles(p: &Project) -> Vec<usize> {
    let (order, n_topo) = topo(p);
    order[n_topo..].to_vec()
}

/// (order, how many leading entries are truly topological)
fn topo(p: &Project) -> (Vec<usize>, usize) {
    let n = p.buses.len();
    let out_of = |i: usize| {
        p.buses[i]
            .output
            .as_deref()
            .and_then(|o| p.bus_index(o).ok())
    };
    let mut indeg = vec![0usize; n];
    for i in 0..n {
        if let Some(d) = out_of(i) {
            if d != i {
                indeg[d] += 1;
            }
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut done = vec![false; n];
    while let Some(i) = (0..n).find(|&i| !done[i] && indeg[i] == 0) {
        done[i] = true;
        order.push(i);
        if let Some(d) = out_of(i) {
            if d != i {
                indeg[d] -= 1;
            }
        }
    }
    let n_topo = order.len();
    order.extend((0..n).filter(|&i| !done[i]));
    (order, n_topo)
}

/// One node of the processing plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlanNode {
    /// Strip id ("trk3", "bus1", "master").
    pub strip: String,
    pub kind: StripKind,
    /// Index into project.tracks / project.buses (None for the master).
    pub index: Option<usize>,
    /// Strip ids summed into this node (outputs and sends), in route order.
    pub inputs: Vec<String>,
    /// Strip ids whose signal keys a device here (sidechains).
    pub keys: Vec<String>,
    /// Devices that process audio, in order (bypassed inserts skipped).
    pub active_devices: Vec<String>,
}

/// The order the renderer processes strips in.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProcessPlan {
    /// Tracks (declaration order), buses (`bus_order`), then the master.
    pub nodes: Vec<PlanNode>,
    /// Bus strip ids caught on a routing cycle (empty for a valid graph).
    pub cycles: Vec<String>,
}

impl ProcessPlan {
    /// Bus indices in processing order (what the renderer iterates).
    pub fn bus_indices(&self) -> Vec<usize> {
        self.nodes
            .iter()
            .filter(|n| n.kind == StripKind::Bus)
            .filter_map(|n| n.index)
            .collect()
    }
}

/// Compile a project into its processing plan.
pub fn compile(p: &Project) -> ProcessPlan {
    compile_state(p, &RackState::from_project(p))
}

/// Compile with an already derived rack state.
pub fn compile_state(p: &Project, rack: &RackState) -> ProcessPlan {
    let nt = p.tracks.len();
    let node = |si: usize, index: Option<usize>| {
        let s = &rack.strips[si];
        PlanNode {
            strip: s.id.clone(),
            kind: s.kind,
            index,
            inputs: rack
                .routes
                .iter()
                .filter(|r| {
                    matches!(r, Route::Output { .. } | Route::Send { .. }) && r.to() == s.id
                })
                .map(|r| r.from().to_string())
                .collect(),
            keys: rack
                .routes
                .iter()
                .filter(|r| matches!(r, Route::Sidechain { .. }) && r.to() == s.id)
                .map(|r| r.from().to_string())
                .collect(),
            active_devices: s
                .devices
                .iter()
                .filter(|d| d.kind == DeviceKind::Instrument || !d.bypassed)
                .map(|d| d.id.clone())
                .collect(),
        }
    };
    let mut nodes: Vec<PlanNode> = (0..nt).map(|i| node(i, Some(i))).collect();
    for bi in bus_order(p) {
        nodes.push(node(nt + bi, Some(bi)));
    }
    nodes.push(node(rack.strips.len() - 1, None));
    let cycles = bus_cycles(p)
        .into_iter()
        .map(|bi| rack.strips[nt + bi].id.clone())
        .collect();
    ProcessPlan { nodes, cycles }
}
