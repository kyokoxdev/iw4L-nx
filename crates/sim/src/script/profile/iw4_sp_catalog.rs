// Hand-maintained, unlike the generated `iw4_catalog`: builtins IW4
// singleplayer scripts call that the multiplayer list lacks. `Catalog::iw4_sp`
// layers these over the multiplayer list. Fill it from `iw4l check-scripts`
// run over the SP zones; see docs/SP.md.
use super::catalog::{Builtin, Namespace::*, Owner::*};

// ponytail: names and developer flags not yet checked against the SP scripts or executable; `check-scripts` confirms them.
pub(super) const IW4_SP: &[Builtin] = &[
    Builtin::new(Function, "cinematicingame", Script, false),
    Builtin::new(Function, "cinematicingameloop", Script, false),
    Builtin::new(Function, "cinematicingameloopresident", Script, false),
    Builtin::new(Function, "cinematicingamesync", Script, false),
    Builtin::new(Function, "iscinematicplaying", Script, false),
    Builtin::new(Function, "stopcinematicingame", Script, false),
];
