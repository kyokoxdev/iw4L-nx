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
        Method "getfractionmaxammo" => Zero;
        Function "getglass" => Undefined;
        Function "getglassarray" => EmptyArray;
        Method "getnormalizedmovement" => ZeroVector;
        Function "getpartname" => Undefined;
        Method "getplayerknifemodel" => Undefined;
        Method "getplayersetting" => One; // only key read: "gameskill"; 1 = Regular
        Method "getplayerviewheight" => Zero;
        Method "getplayerweaponmodel" => Undefined;
        // ponytail: every wheel on dirt; trace the surface under the wheel tag for tread fx.
        Method "getwheelsurface" => Dirt;
        Method "hidepart_allinstances" => Undefined;
        Function "isexplosivedamagemod" => Zero;
        Method "joltbody" => Undefined;
        Method "linktoblendtotag" => Undefined;
        Function "physicsjitter" => Undefined;
        Method "playerlinktoblend" => Undefined;
        Method "playrumblelooponentity" => Undefined;
        Method "restoredefaultdroppitch" => Undefined;
        Method "resumespeed" => Undefined;
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
        Function "weaponhasthermalscope" => Zero;
        Function "badplace_arc" => Undefined;
        Function "badplace_brush" => Undefined;
        Function "badplace_cylinder" => Undefined;
        Function "badplace_delete" => Undefined;
        Function "bulletspread" => Undefined;
        Function "commitsave" => Undefined;
        Function "commitwouldbevalid" => One;
        Function "connectpaths" => Undefined;
        Function "disconnectpaths" => Undefined;
        Function "freeentitysentient" => Undefined;
        Function "getcommandfromkey" => EmptyString;
        Function "getcorpsearray" => EmptyArray;
        Function "getkeybinding" => Unbound;
        Function "getweaponclipmodel" => EmptyString;
        Function "isenemyteam" => Zero;
        Function "issaverecentlyloaded" => Zero;
        Function "issavesuccessful" => One;
        Function "isturretactive" => Zero;
        Function "kill" => Undefined;
        Function "laserforceoff" => Undefined;
        Function "laserforceon" => Undefined;
        Function "makeentitysentient" => Undefined;
        Function "notifyoncommand" => Undefined;
        Function "objective_additionalposition" => Undefined;
        Function "objective_string" => Undefined;
        Function "refreshhudcompass" => Undefined;
        Function "savegame" => One;
        Function "savegamenocommit" => One;
        Function "setblur" => Undefined;
        Function "setdebugangles" => Undefined;
        Function "setdebugorigin" => Undefined;
        Function "setsunlight" => Undefined;
        Function "settimescale" => Undefined;
        Function "target_set" => Undefined;
        Function "target_setjavelinonly" => Undefined;
        Function "updategamerprofile" => Undefined;
        Function "updategamerprofileall" => Undefined;
        Method "addaieventlistener" => Undefined;
        Method "allowcrouch" => Undefined;
        Method "allowedstances" => Undefined;
        Method "allowprone" => Undefined;
        Method "allowstand" => Undefined;
        Method "atdangerousnode" => Zero;
        Method "canuseturret" => Zero;
        Method "checkcoverexitposwithpath" => Undefined;
        Method "checkgrenadethrow" => Zero;
        Method "checkgrenadethrowpos" => Zero;
        Method "clearfixednodesafevolume" => Undefined;
        Method "comparenodedirtopathdir" => Zero;
        Method "connectpaths" => Undefined;
        Method "deactivateeq" => Undefined;
        Method "disableinvulnerability" => Undefined;
        Method "disconnectpaths" => Undefined;
        Method "dismountvehicle" => Undefined;
        Method "dontinterpolate" => Undefined;
        Method "enabledeathshield" => Undefined;
        Method "enablehealthshield" => Undefined;
        Method "enableinvulnerability" => Undefined;
        Method "enterprone" => Undefined;
        Method "eqoff" => Undefined;
        Method "eqon" => Undefined;
        Method "exitprone" => Undefined;
        Method "findreacquiredirectpath" => Undefined;
        Method "flagenemyunattackable" => Undefined;
        Method "getanglestolikelyenemypath" => Undefined;
        Method "getcentroid" => Origin;
        Method "getdebugeye" => Origin;
        Method "getdoorpathnode" => Undefined;
        Method "getdroptofloorposition" => Origin;
        Method "getentnum" => Undefined;
        Method "getlightcolor" => Undefined;
        Method "getmuzzlesideoffsetpos" => Origin;
        Method "getplayerintelisfound" => Zero;
        Method "getturret" => Undefined;
        Method "getvalidcoverpeekouts" => EmptyArray;
        Method "giveachievement" => Undefined;
        Method "isgrenadepossafe" => Zero;
        Method "isindoor" => Zero;
        Method "isinscriptedstate" => Zero;
        Method "isknownenemyinradius" => Zero;
        Method "isknownenemyinvolume" => Zero;
        Method "ismovesuppressed" => Zero;
        Method "isstanceallowed" => One;
        Method "iswaitingonsound" => Zero;
        Method "laserforceoff" => Undefined;
        Method "laserforceon" => Undefined;
        Method "magicgrenademanual" => Undefined;
        Method "makeentitysentient" => Undefined;
        Method "makefakeai" => Undefined;
        Method "maymovefrompointtopoint" => Zero;
        Method "melee" => Undefined;
        Method "mountvehicle" => Undefined;
        Method "nearclaimnode" => Undefined;
        Method "nearclaimnodeandangle" => Undefined;
        Method "painvisionoff" => Undefined;
        Method "painvisionon" => Undefined;
        Method "playerclearstreamorigin" => Undefined;
        Method "playersetstreamorigin" => Undefined;
        Method "pushplayer" => Undefined;
        Method "reacquiremove" => Undefined;
        Method "reacquirestep" => Undefined;
        Method "safeteleport" => Undefined;
        Method "setentitytarget" => Undefined;
        Method "seteq" => Undefined;
        Method "seteqbands" => Undefined;
        Method "seteqlerp" => Undefined;
        Method "setfixednodesafevolume" => Undefined;
        Method "setfriendlychain" => Undefined;
        Method "setlightcolor" => Undefined;
        Method "setlookatentity" => Undefined;
        Method "setlookattext" => Undefined;
        Method "setplayerintelfound" => Undefined;
        Method "setproneanimnodes" => Undefined;
        Method "setruntopos" => Undefined;
        Method "setsoundblend" => Undefined;
        Method "setturretignoregoals" => Undefined;
        Method "shouldfacemotion" => Zero;
        Method "startcoverarrival" => Undefined;
        Method "startragdollfromimpact" => Undefined;
        Method "startusingheroonlylighting" => Undefined;
        Method "startusinglessfrequentlighting" => Undefined;
        Method "stopsoundchannel" => Undefined;
        Method "stopuseturret" => Undefined;
        Method "thermaldrawenable" => Undefined;
        Method "throwgrenade" => Undefined;
        Method "updateplayersightaccuracy" => Undefined;
        Method "updateprone" => Undefined;
        Method "uploadscore" => Undefined;
        Method "uploadtime" => Undefined;
        Method "useturret" => Undefined;
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
        Function "magicgrenade" => Undefined;
        Function "magicgrenademanual" => Undefined;
        Function "objective_setpointertextoverride" => Undefined;
        Function "objective_string_nomessage" => Undefined;
        Function "precachenightvisioncodeassets" => Undefined;
        Function "resetsundirection" => Undefined;
        Function "resetsunlight" => Undefined;
        Function "setculldist" => Undefined;
        Function "sethalfresparticles" => Undefined;
        Function "soundsettimescalefactor" => Undefined;
        Function "target_hidefromplayer" => Undefined;
        Function "target_isinrect" => Zero;
        Function "target_setattackmode" => Undefined;
        Function "target_setoffscreenshader" => Undefined;
        Function "target_setshader" => Undefined;
        Function "target_showtoplayer" => Undefined;
        Method "allowfire" => Undefined;
        Method "allowmelee" => Undefined;
        Method "castshadows" => Undefined;
        Method "disablebreaching" => Undefined;
        Method "disableturretdismount" => Undefined;
        Method "dontcastshadows" => Undefined;
        Method "enablebreaching" => Undefined;
        Method "forceviewmodelanimation" => Undefined;
        Method "freeentitysentient" => Undefined;
        Method "getenemyinfo" => Undefined;
        Method "hideonclient" => Undefined;
        Method "hideviewmodel" => Undefined;
        Method "invisiblenotsolid" => Undefined;
        Method "magicgrenade" => Undefined;
        Method "nightvisionforceoff" => Undefined;
        Method "playersetgroundreferenceent" => Undefined;
        Method "setcorpseremovetimer" => Undefined;
        Method "setengagementmaxdist" => Undefined;
        Method "setengagementmindist" => Undefined;
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
        Method "lerpviewangleclamp" => Undefined;
        Function "objective_current" => Undefined;
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
        Method "useby" => Undefined;
        Method "vehicle_getbodyvelocity" => ZeroVector;
        Method "vehicle_getsteering" => Zero;
        Method "vehicle_getthrottle" => Zero;
        Method "vehicle_turnengineon" => Undefined;
        Method "vehphys_disablecrashing" => Undefined;
        Method "vehphys_enablecrashing" => Undefined;
        Method "vehphys_launch" => Undefined;
        Method "vehphys_setconveyorbelt" => Undefined;
    }
}
