//! `aetherrv-can-aux -c /etc/aetherrv/aetherrv.toml --bus <name>` (or `--list` to print the aux buses)

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-can-aux", args.verbose);
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    if args.rest.first().map(String::as_str) == Some("--list") {
        for b in aetherrv_can_aux::aux_buses(&cfg) {
            println!("{b}");
        }
        return Ok(());
    }
    let bus = match args.rest.as_slice() {
        [flag, b, ..] if flag == "--bus" => b.clone(),
        _ => anyhow::bail!("usage: aetherrv-can-aux -c <config> --bus <name> | --list"),
    };
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(aetherrv_can_aux::run(cfg, bus, None))
}
