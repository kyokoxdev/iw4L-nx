//! iw4l-bake: offline deko3d shader bake (docs/deko3d_implementation_plan.md).
mod uam;
mod zones;

/// The DKSH bytes are what devkitPro's `uam` 1.1.0 CLI writes for the GLSL.
const SELFTEST: [(&str, uam::Stage, &str, &[u8]); 2] = [
    (
        "vert",
        uam::Stage::Vertex,
        include_str!("../selftest/deko3d_sample.vert.glsl"),
        include_bytes!("../selftest/deko3d_sample.vert.dksh"),
    ),
    (
        "frag",
        uam::Stage::Fragment,
        include_str!("../selftest/deko3d_sample.frag.glsl"),
        include_bytes!("../selftest/deko3d_sample.frag.dksh"),
    ),
];

fn selftest() -> Result<(), String> {
    for (name, stage, glsl, expected) in SELFTEST {
        let dksh = uam::compile(glsl, stage).map_err(|rc| format!("{name}: uam rc {rc}"))?;
        if dksh != expected {
            return Err(format!(
                "{name}: DKSH differs from devkitPro uam ({} vs {} bytes)",
                dksh.len(),
                expected.len()
            ));
        }
        println!("{name}: {} bytes, matches devkitPro uam", dksh.len());
    }
    Ok(())
}

fn bake(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut zones = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            // SAFETY: still single-threaded; the loaders read IW4L_GAMES.
            "--games" => unsafe { std::env::set_var("IW4L_GAMES", value()?) },
            "--zones" => zones = Some(value()?.split(',').map(str::to_owned).collect()),
            _ => return Err(USAGE.into()),
        }
    }
    let games = asset_transport::games_root_from_env()?;
    // ponytail: MP maps only; SP stems need find_zone_file_version and the
    // campaign zone set, add with the SP release gate.
    let zones: Vec<String> = zones.unwrap_or_else(|| asset_transport::list_mp_maps(&games));
    zones::run(&games, &zones)
}

const USAGE: &str = "usage: iw4l-bake selftest | iw4l-bake [--games <dir>] [--zones a,b]";

fn main() -> std::process::ExitCode {
    zones::force_switch_abi();
    let mut args = std::env::args().skip(1).peekable();
    let result = if args.peek().map(String::as_str) == Some("selftest") {
        selftest()
    } else {
        bake(args)
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}
