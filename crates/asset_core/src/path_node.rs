/// One SP AI path node from a level's `PathData`, as scripts see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathNode {
    /// Script `type`: "Path", "Cover Left", "Begin", ...
    pub kind: &'static str,
    pub origin: [f32; 3],
    pub yaw: f32,
    pub radius: f32,
    pub spawnflags: i32,
    /// `targetname`, `target`, `script_linkname`, `script_noteworthy`,
    /// `animscript`, when the node has them.
    pub keys: Vec<(&'static str, String)>,
    /// Linked node index and path distance.
    pub links: Vec<(u16, f32)>,
}

/// IW4 `nodeType` order; the script `type` of each node.
pub const IW4_NODE_TYPES: [&str; 20] = [
    "BAD NODE",
    "Path",
    "Cover Stand",
    "Cover Crouch",
    "Cover Crouch Window",
    "Cover Prone",
    "Cover Right",
    "Cover Left",
    "Ambush",
    "Exposed",
    "Conceal Stand",
    "Conceal Crouch",
    "Conceal Prone",
    "Door",
    "Door Interior",
    "Scripted",
    "Begin",
    "End",
    "Turret",
    "Guard",
];
