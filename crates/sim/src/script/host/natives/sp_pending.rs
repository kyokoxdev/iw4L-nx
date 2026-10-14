//! IW4 builtins singleplayer scripts call that are not built yet. Each one returns a typed default
//! instead of faulting, so level scripts keep running; the first call of each
//! is logged and every call counts in `RoundScript::unsupported`.
use bevy_ecs::prelude::World;

use crate::script::host::{arrays::new_array, players::entity_field};
use crate::script::{Namespace, NativeRegistry, RoundScript, Value};

enum Stub {
    Undefined,
    Zero,
    One,
    EmptyArray,
    ZeroVector,
    EmptyString,
    /// The surface name `getwheelsurface` answers.
    Dirt,
    /// `getkeybinding` for an unbound command: `["count"] = 0`.
    Unbound,
    /// The receiver's origin.
    Origin,
    /// An argument passed through (`getstartorigin` returns its origin).
    Arg(usize),
}

fn pending(
    world: &mut World,
    name: &'static str,
    default: Stub,
    receiver: &Value,
    args: &[Value],
) -> Result<Value, String> {
    let calls = {
        let mut runtime = world.resource_mut::<RoundScript>();
        let calls = runtime.unsupported.entry(name).or_default();
        *calls = calls.saturating_add(1);
        *calls
    };
    if calls == 1 {
        diag::info!(Sim, "gsc: campaign builtin {name} not built yet; stubbed");
    }
    Ok(match default {
        Stub::Undefined => Value::Undefined,
        Stub::Zero => Value::Int(0),
        Stub::One => Value::Int(1),
        Stub::EmptyArray => return new_array(world, Vec::new()),
        Stub::ZeroVector => Value::Vector([0.0; 3]),
        Stub::EmptyString => Value::string(""),
        Stub::Dirt => Value::string("dirt"),
        Stub::Unbound => {
            let array = new_array(world, Vec::new())?;
            if let Value::Array(id) = array {
                world
                    .resource_mut::<RoundScript>()
                    .arrays
                    .get_mut(&id)
                    .unwrap()
                    .insert(
                        crate::script::ArrayKey::String("count".into()),
                        Value::Int(0),
                    );
            }
            array
        }
        Stub::Origin => match receiver {
            Value::Object(id) => entity_field(world, *id, "origin"),
            _ => Value::Undefined,
        },
        Stub::Arg(index) => args.get(index).cloned().unwrap_or(Value::Undefined),
    })
}

macro_rules! stubs {
    ($registry:ident; $($namespace:ident $name:literal => $default:expr;)*) => {
        $($registry.register(Namespace::$namespace, $name, |world, receiver, args| {
            pending(world, $name, $default, receiver, args)
        });)*
    };
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    use Stub::*;
    stubs! { registry;
        Method "deactivatereverb" => Undefined;
        Function "destroyglass" => Undefined;
        Method "disableaimassist" => Undefined;
        Method "enableaimassist" => Undefined;
        Method "freevehicle" => Undefined;
        Method "getattachpos" => Origin;
        Function "getglass" => Undefined;
        Function "getglassarray" => EmptyArray;
        Function "getpartname" => Undefined;
        Method "getplayerknifemodel" => Undefined;
        Method "getplayersetting" => One; // only key read: "gameskill"; 1 = Regular
        Method "getplayerweaponmodel" => Undefined;
        // ponytail: every wheel on dirt; trace the surface under the wheel tag for tread fx.
        Method "getwheelsurface" => Dirt;
        Method "hidepart_allinstances" => Undefined;
        Method "joltbody" => Undefined;
        Function "physicsjitter" => Undefined;
        Method "playrumblelooponentity" => Undefined;
        Method "restoredefaultdroppitch" => Undefined;
        Method "setairresistance" => Undefined;
        Method "setreverb" => Undefined;
        Method "setswitchnode" => Undefined;
        Method "setvehiclelookattext" => Undefined;
        Method "setwaitspeed" => Undefined;
        Method "setwatersheeting" => Undefined;
        Method "setwaypointedgestyle_rotatingicon" => Undefined;
        Method "setwaypointiconoffscreenonly" => Undefined;
        Method "vehicle_isphysveh" => Zero;
        Method "vehicle_turnengineoff" => Undefined;
        Method "vehphys_crash" => Undefined;
        Method "vehphys_setspeed" => Undefined;
        Method "vibrate" => Undefined;
        Function "badplace_arc" => Undefined;
        Function "freeentitysentient" => Undefined;
        Function "getcommandfromkey" => EmptyString;
        Function "getcorpsearray" => EmptyArray;
        Function "getkeybinding" => Unbound;
        Function "getweaponclipmodel" => EmptyString;
        Function "issavesuccessful" => One;
        Function "isturretactive" => Zero;
        Function "kill" => Undefined;
        Function "laserforceoff" => Undefined;
        Function "laserforceon" => Undefined;
        Function "makeentitysentient" => Undefined;
        Function "refreshhudcompass" => Undefined;
        Function "setdebugangles" => Undefined;
        Function "setdebugorigin" => Undefined;
        Function "setsunlight" => Undefined;
        Function "settimescale" => Undefined;
        Function "target_setjavelinonly" => Undefined;
        Function "updategamerprofile" => Undefined;
        Function "updategamerprofileall" => Undefined;
        Method "clearfixednodesafevolume" => Undefined;
        Method "deactivateeq" => Undefined;
        Method "dontinterpolate" => Undefined;
        Method "eqoff" => Undefined;
        Method "eqon" => Undefined;
        Method "getcentroid" => Origin;
        Method "getdebugeye" => Origin;
        Method "getlightcolor" => Undefined;
        Method "getmuzzlesideoffsetpos" => Origin;
        Method "getplayerintelisfound" => Zero;
        Method "giveachievement" => Undefined;
        Method "laserforceoff" => Undefined;
        Method "laserforceon" => Undefined;
        Method "makefakeai" => Undefined;
        Method "painvisionoff" => Undefined;
        Method "painvisionon" => Undefined;
        Method "playerclearstreamorigin" => Undefined;
        Method "playersetstreamorigin" => Undefined;
        Method "seteq" => Undefined;
        Method "seteqbands" => Undefined;
        Method "seteqlerp" => Undefined;
        Method "setfixednodesafevolume" => Undefined;
        Method "setfriendlychain" => Undefined;
        Method "setlightcolor" => Undefined;
        Method "setlookatentity" => Undefined;
        Method "setlookattext" => Undefined;
        Method "setplayerintelfound" => Undefined;
        Method "setsoundblend" => Undefined;
        Method "setturretignoregoals" => Undefined;
        Method "startragdollfromimpact" => Undefined;
        Method "startusingheroonlylighting" => Undefined;
        Method "startusinglessfrequentlighting" => Undefined;
        Method "stopsoundchannel" => Undefined;
        Method "thermaldrawenable" => Undefined;
        Method "uploadscore" => Undefined;
        Method "uploadtime" => Undefined;
        Function "bullettracer" => Undefined;
        Function "clearallcorpses" => Undefined;
        Function "disableforcedsunshadows" => Undefined;
        Function "disableouterspacemodellighting" => Undefined;
        Function "enableforcednosunshadows" => Undefined;
        Function "enableforcedsunshadows" => Undefined;
        Function "enableouterspacemodellighting" => Undefined;
        Function "getculldist" => Zero;
        Function "getmapsunlight" => ZeroVector;
        Function "getnodesinradius" => EmptyArray;
        Function "gettimesincelastpaused" => Zero;
        Function "hideonclient" => Undefined;
        Function "precachenightvisioncodeassets" => Undefined;
        Function "resetsundirection" => Undefined;
        Function "resetsunlight" => Undefined;
        Function "setculldist" => Undefined;
        Function "sethalfresparticles" => Undefined;
        Function "soundsettimescalefactor" => Undefined;
        Function "target_hidefromplayer" => Undefined;
        Function "target_setattackmode" => Undefined;
        Function "target_setoffscreenshader" => Undefined;
        Function "target_setshader" => Undefined;
        Function "target_showtoplayer" => Undefined;
        Method "castshadows" => Undefined;
        Method "disablebreaching" => Undefined;
        Method "disableturretdismount" => Undefined;
        Method "dontcastshadows" => Undefined;
        Method "enablebreaching" => Undefined;
        Method "forceviewmodelanimation" => Undefined;
        Method "hideonclient" => Undefined;
        Method "hideviewmodel" => Undefined;
        Method "invisiblenotsolid" => Undefined;
        Method "nightvisionforceoff" => Undefined;
        Method "playersetgroundreferenceent" => Undefined;
        Method "setcorpseremovetimer" => Undefined;
        Method "setlightfovrange" => Undefined;
        Method "setpotentialthreat" => Undefined;
        Method "settalktospecies" => Undefined;
        Method "setturretanim" => Undefined;
        Method "setturretcanaidetach" => Undefined;
        Method "showonclient" => Undefined;
        Method "showviewmodel" => Undefined;
        Method "stoplookat" => Undefined;
        Method "visiblesolid" => Undefined;
        // MP-catalog names other campaign levels reach with no native yet.
        Function "deleteglass" => Undefined;
        Method "getbarrelspinrate" => Zero;
        Function "getgroundposition" => Arg(0);
        Method "getmode" => Undefined;
        Method "hideallparts" => Undefined;
        Function "physicsexplosioncylinder" => Undefined;
        Function "physicsjolt" => Undefined;
        Method "setaispread" => Undefined;
        Method "setautorotationdelay" => Undefined;
        Method "setmotiontrackervisible" => Undefined;
        Function "setnorthyaw" => Undefined;
        Method "setplayerspread" => Undefined;
        Method "setstablemissile" => Undefined;
        Method "setsuppressiontime" => Undefined;
        Method "startbarrelspin" => Undefined;
        Function "stopallrumbles" => Undefined;
        Method "stopbarrelspin" => Undefined;
        Function "transformmove" => Undefined;
        Method "vehicle_getbodyvelocity" => ZeroVector;
        Method "vehicle_turnengineon" => Undefined;
        Method "vehphys_disablecrashing" => Undefined;
        Method "vehphys_enablecrashing" => Undefined;
        Method "vehphys_launch" => Undefined;
        Method "vehphys_setconveyorbelt" => Undefined;
    }
}
