mod uam;

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

fn main() -> std::process::ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("selftest") => selftest(),
        _ => Err("usage: iw4l-bake selftest".into()),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}
