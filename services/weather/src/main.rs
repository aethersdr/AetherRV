//! `aetherrv-weather -c /etc/aetherrv/aetherrv.toml`
//!
//! `aetherrv-weather flow <frame dir A> <frame dir B> [--intensity <prefix>]` prints the motion field between two
//! recorded frames (the `flow.json` the service would write) and how long it took, for comparing against the
//! Python field. It writes nothing into the frames; `--intensity` saves the two preprocessed images as
//! `<prefix>-a.pgm` / `<prefix>-b.pgm`.

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-weather", args.verbose);
    if args.rest.first().map(String::as_str) == Some("flow") {
        return flow_cmd(&args.rest[1..]);
    }
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    let opts = aetherrv_weather::Options::from_config(&cfg);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(aetherrv_weather::run(cfg, opts))
}

fn flow_cmd(a: &[String]) -> anyhow::Result<()> {
    use aetherrv_weather::flow;
    let (da, db, dump) = match a {
        [da, db] => (da, db, None),
        [da, db, opt, prefix] if opt == "--intensity" => (da, db, Some(prefix)),
        _ => anyhow::bail!("usage: aetherrv-weather flow <frame dir A> <frame dir B> [--intensity <prefix>]"),
    };
    let (da, db) = (std::path::Path::new(da), std::path::Path::new(db));
    if let Some(prefix) = dump {
        let o = flow::origin(da, flow::FLOW_Z)?.ok_or_else(|| anyhow::anyhow!("no z{} block in A", flow::FLOW_Z))?;
        for (d, tag) in [(da, "a"), (db, "b")] {
            let g = flow::intensity(d, o, 3)?;
            let mut pgm = format!("P5\n{} {}\n255\n", g.w, g.h).into_bytes();
            pgm.extend(g.d.iter().map(|v| *v as u8));
            std::fs::write(format!("{prefix}-{tag}.pgm"), pgm)?;
        }
    }
    let t = |p: &std::path::Path| p.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<i64>().ok());
    let dt = t(da).zip(t(db)).map_or(0, |(a, b)| b - a);
    let t0 = std::time::Instant::now();
    let f = flow::flow_pair(da, db)?;
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    match f {
        Some(f) => println!("{}", f.to_json(dt)),
        None => println!("null"),
    }
    eprintln!("flow_pair: {ms:.0} ms");
    Ok(())
}
